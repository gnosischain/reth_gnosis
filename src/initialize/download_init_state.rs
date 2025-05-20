use anyhow::{bail, Context};
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{BufReader, BufWriter, Read as _, Write as _};
use std::time::Duration;
use std::{fs::File, path::Path};
use tokio::{fs, io::AsyncWriteExt};
use zstd::Decoder;

#[derive(Debug, Clone, Copy)]
pub struct DownloadStateSpec {
    pub expected_state_root: &'static str,
    pub block_num: &'static str,
    pub total_difficulty: &'static str,
    pub header_hash: &'static str,
}

pub const GNOSIS_DOWNLOAD_SPEC: DownloadStateSpec = DownloadStateSpec {
    expected_state_root: "0x95c4ecc49287d652e956b71ef82fb34a17da87fcbd6ab64f05542ddd3b5cb44f",
    block_num: "26478650",
    total_difficulty: "8626000110427540000000000000000000000000000000",
    header_hash: "a133198478cb01b4585604d07f584633f1f147103b49672d2bd87a5a3ba2c06e",
};

pub const CHIADO_DOWNLOAD_SPEC: DownloadStateSpec = DownloadStateSpec {
    expected_state_root: "0x90b1762d6b81ea05b51aea094a071f7ec4c0742e2bb2d5d560d1833443ff40fd",
    block_num: "700000",
    total_difficulty: "231708131825107706987652208063906496124457284",
    header_hash: "cdc424294195555e53949b6043339a49b049b48caa8d85bc7d5f5d12a85964b6",
};

// ────────────────────────────────────────────────────────────
// Download/combine Gnosis state before the node boots
// ────────────────────────────────────────────────────────────
const LFS_BATCH: &str = "https://github.com/gnosischain/reth-init-state.git/info/lfs/objects/batch";

// Chunk OIDs and sizes
const GNOSIS_CHUNKS: [(&str, u64); 2] = [
    (
        "610f2c013d695a1f60ccbdc2b125436121ab71be050dd18ba31fbfa214e7072f",
        4_294_967_296,
    ),
    (
        "a77414288d24c2e23685dbb3e6e53a21a6d1e5d6839c0cd60ed8a0eff3815cdd",
        1_377_976_148,
    ),
];

const CHIADO_CHUNKS: [(&str, u64); 1] = [(
    "3b30b1e0ace67b6f3ac1735fa19dc3ddc6327d04dabf748b8296dbb4b7c69fdf",
    22_468_068,
)];

fn get_chunks(chain: &str) -> Vec<(&'static str, u64)> {
    match chain {
        "gnosis" => GNOSIS_CHUNKS.to_vec(),
        "chiado" => CHIADO_CHUNKS.to_vec(),
        _ => unreachable!(),
    }
}

const HEADER_FILE: &str = "header.rlp";

fn get_header_url(chain: &str) -> &'static str {
    match chain {
        "gnosis" => "https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/gnosis/header_26478650.rlp",
        "chiado" => "https://media.githubusercontent.com/media/gnosischain/reth-init-state/refs/heads/main/chiado/header_700000.rlp",
        _ => unreachable!(),
    }
}

const STATE_FILE: &str = "state.jsonl";

fn get_state_size(chain: &str) -> u64 {
    match chain {
        "gnosis" => 27_498_292_407,
        "chiado" => 111_610_557,
        _ => unreachable!(),
    }
}

fn decompress_zstd(input_path: &str, output_path: &str, expected_size: u64) -> std::io::Result<()> {
    let input_file = File::open(input_path)?;
    let reader = BufReader::new(input_file);
    let mut decoder = Decoder::new(reader)?;

    let pb = ProgressBar::new(expected_size);
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.magenta/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("=>-"));

    let mut output_file = BufWriter::new(File::create(output_path)?);
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = decoder.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        output_file.write_all(&buffer[..bytes_read])?;
        pb.inc(bytes_read as u64);
    }

    pb.finish_with_message("✅ State Decompressed");
    Ok(())
}

/// Downloads the initial state
pub async fn ensure_state(data_dir: &Path, chain: &str) -> anyhow::Result<()> {
    fs::create_dir_all(data_dir).await?;

    // remove any *.part leftovers
    let mut rd = fs::read_dir(data_dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        if entry.file_name().to_string_lossy().ends_with(".part") {
            fs::remove_file(entry.path()).await.ok();
        }
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()?;

    // download/verify each chunk
    for (idx, (oid, size)) in get_chunks(chain).iter().enumerate() {
        let name = format!("chunk_{idx:02}");
        let out = data_dir.join(&name);
        if file_has_size(&out, *size).await? {
            println!("✅  {name} already complete");
            continue;
        }

        println!("⬇️   downloading {name} …");

        let batch_req = serde_json::json!({
            "operation": "download",
            "transfer": ["basic"],
            "objects": [{ "oid": oid, "size": size }]
        });

        #[derive(serde::Deserialize)]
        struct Batch {
            objects: Vec<Item>,
        }
        #[derive(serde::Deserialize)]
        struct Item {
            actions: Act,
        }
        #[derive(serde::Deserialize)]
        struct Act {
            download: Href,
        }
        #[derive(serde::Deserialize)]
        struct Href {
            href: String,
        }

        let href = client
            .post(LFS_BATCH)
            .json(&batch_req)
            .header("Accept", "application/vnd.git-lfs+json")
            .send()
            .await?
            .error_for_status()?
            .json::<Batch>()
            .await?
            .objects
            .first()
            .map(|o| &o.actions.download.href)
            .context("missing download URL")?
            .to_owned();

        // total size is known beforehand (`size`), so build a bar with that length
        let pb = ProgressBar::new(*size);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} {bytes}/{total_bytes} [{bar:40.cyan/blue}] {bytes_per_sec} ETA {eta}",
            )
            .unwrap()
            .progress_chars("#>-"),
        );

        let tmp = out.with_extension("part");
        let mut file = fs::File::create(&tmp).await?;
        let mut resp = client.get(href).send().await?.error_for_status()?;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk).await?;
            pb.inc(chunk.len() as u64);
        }
        file.flush().await?;
        fs::rename(&tmp, &out).await?;
        if !file_has_size(&out, *size).await? {
            bail!("size mismatch for {name}");
        }
    }

    println!("✅  all chunks present");

    // header
    let header_path = data_dir.join(HEADER_FILE);
    if !header_path.exists() {
        println!("⬇️   downloading header …");
        let bytes = client
            .get(get_header_url(chain))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        fs::write(&header_path, &bytes).await?;
    }

    // combine chunks
    let state_path = data_dir.join(STATE_FILE);
    if file_has_size(&state_path, get_state_size(chain)).await? {
        println!("✅  state already complete");
        return Ok(());
    }

    println!("🛠   combining chunks → {STATE_FILE}");
    let tmp = state_path.with_extension(".zst.part");
    let mut out = fs::File::create(&tmp).await?;
    for idx in 0..get_chunks(chain).len() {
        let mut f = fs::File::open(data_dir.join(format!("chunk_{idx:02}"))).await?;
        tokio::io::copy(&mut f, &mut out).await?;
    }
    out.flush().await?;
    fs::rename(&tmp, &state_path.with_extension("zst")).await?;

    println!("🛠   decompressing state");
    decompress_zstd(
        state_path.with_extension("zst").to_str().unwrap(),
        state_path.with_extension("part").to_str().unwrap(),
        get_state_size(chain),
    )?;
    println!("✅  full state written");

    fs::remove_file(state_path.with_extension("zst")).await?;
    fs::rename(state_path.with_extension("part"), &state_path).await?;

    if !file_has_size(&state_path, get_state_size(chain)).await? {
        bail!("combined state size mismatch");
    }

    println!("✅  state + header ready");
    Ok(())
}

async fn file_has_size(path: &Path, expected: u64) -> anyhow::Result<bool> {
    Ok(tokio::fs::metadata(path)
        .await
        .map(|m| m.len() == expected)
        .unwrap_or(false))
}
