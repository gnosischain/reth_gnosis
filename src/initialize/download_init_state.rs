use anyhow::{bail, Context};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use std::time::Duration;
use tokio::{fs, io::AsyncWriteExt};

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

const GNOSIS_OIDS: [&str; 7] = [
    "cd3b4b0edc6fc86bd9eee682ed0c6a1cc9ddc90fde12c855f960baf6ad74f11b",
    "3c591add3562c42baa113623418bb6f51fb73f183a866a30a372be52206d54c3",
    "4a7be543b8c2bd00e4a2b51ae35e065c29ddbb38becb62c42199a15d56f0d432",
    "c8ea30f3b2a065485cd568ae384f80abdb970ed99cf46666e106a613e7903743",
    "db2a3aa71490295a9de55c80fcb8097981079c5acedb9fc01aebdf9a0fd7d480",
    "eeec94bee7c49f0c2de2d2bf608d96ac0e870f9819e53edd738fff8467bde6ad",
    "ad2ecfba180f5da124d342134f766c4ab90280473e487f7f3eb73d19bf7598b1",
];

const GNOSIS_SIZES: [u64; 7] = [
    4_294_967_296,
    4_294_967_296,
    4_294_967_296,
    4_294_967_296,
    4_294_967_296,
    4_294_967_296,
    1_728_488_631,
];

const CHIADO_OIDS: [&str; 1] = ["11046652a6ec2c84c201503200bd0e8f05ce79d0399a677c7244471a21bac35f"];

const CHIADO_SIZES: [u64; 1] = [111_610_557];

fn get_oids(chain: &str) -> &'static [&'static str] {
    match chain {
        "gnosis" => &GNOSIS_OIDS,
        "chiado" => &CHIADO_OIDS,
        _ => unreachable!(),
    }
}

fn get_sizes(chain: &str) -> &'static [u64] {
    match chain {
        "gnosis" => &GNOSIS_SIZES,
        "chiado" => &CHIADO_SIZES,
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
    for (idx, (&oid, &size)) in get_oids(chain).iter().zip(get_sizes(chain)).enumerate() {
        let name = format!("chunk_{idx:02}");
        let out = data_dir.join(&name);
        if file_has_size(&out, size).await? {
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
        let pb = ProgressBar::new(size);
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
        if !file_has_size(&out, size).await? {
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

    println!("🛠   combining chunks → {}", STATE_FILE);
    let tmp = state_path.with_extension("part");
    let mut out = fs::File::create(&tmp).await?;
    for idx in 0..get_oids(chain).len() {
        let mut f = fs::File::open(data_dir.join(format!("chunk_{idx:02}"))).await?;
        tokio::io::copy(&mut f, &mut out).await?;
    }
    out.flush().await?;
    fs::rename(&tmp, &state_path).await?;
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
