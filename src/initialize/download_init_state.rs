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
// R2 hosted state files
// ────────────────────────────────────────────────────────────
const R2_BASE: &str = "https://initstate.gnosischain.com";

fn get_state_url(chain: &str) -> String {
    match chain {
        "gnosis" => format!("{R2_BASE}/gnosis/compressed_state_26478650.jsonl.zst"),
        "chiado" => format!("{R2_BASE}/chiado/compressed_state_700000.jsonl.zst"),
        _ => unreachable!(),
    }
}

fn get_header_url(chain: &str) -> String {
    match chain {
        "gnosis" => format!("{R2_BASE}/gnosis/header_26478650.rlp"),
        "chiado" => format!("{R2_BASE}/chiado/header_700000.rlp"),
        _ => unreachable!(),
    }
}

/// Compressed .zst file size
fn get_compressed_state_size(chain: &str) -> u64 {
    match chain {
        "gnosis" => 5_672_943_444,
        "chiado" => 22_468_068,
        _ => unreachable!(),
    }
}

/// Decompressed state.jsonl file size
fn get_decompressed_state_size(chain: &str) -> u64 {
    match chain {
        "gnosis" => 27_498_292_407,
        "chiado" => 111_610_557,
        _ => unreachable!(),
    }
}

const HEADER_FILE: &str = "header.rlp";
const STATE_FILE: &str = "state.jsonl";
const COMPRESSED_STATE_FILE: &str = "state.jsonl.zst";

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

/// Downloads a file with progress bar
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_size: u64,
) -> anyhow::Result<()> {
    let tmp = dest.with_extension("part");

    let pb = ProgressBar::new(expected_size);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} {bytes}/{total_bytes} [{bar:40.cyan/blue}] {bytes_per_sec} ETA {eta}",
        )
        .unwrap()
        .progress_chars("#>-"),
    );

    let mut file = fs::File::create(&tmp).await?;
    let mut resp = client.get(url).send().await?.error_for_status()?;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk).await?;
        pb.inc(chunk.len() as u64);
    }
    file.flush().await?;
    pb.finish();

    fs::rename(&tmp, dest).await?;
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

    let state_path = data_dir.join(STATE_FILE);
    let compressed_path = data_dir.join(COMPRESSED_STATE_FILE);
    let header_path = data_dir.join(HEADER_FILE);

    // Check if final decompressed state already exists
    if file_has_size(&state_path, get_decompressed_state_size(chain)).await? {
        println!("✅  state already complete");
    } else {
        // Download compressed state if needed
        if !file_has_size(&compressed_path, get_compressed_state_size(chain)).await? {
            println!("⬇️   downloading compressed state …");
            download_file(
                &client,
                &get_state_url(chain),
                &compressed_path,
                get_compressed_state_size(chain),
            )
            .await
            .context("failed to download compressed state")?;

            if !file_has_size(&compressed_path, get_compressed_state_size(chain)).await? {
                bail!("compressed state size mismatch");
            }
            println!("✅  compressed state downloaded");
        } else {
            println!("✅  compressed state already present");
        }

        // Decompress
        println!("🛠   decompressing state …");
        decompress_zstd(
            compressed_path.to_str().unwrap(),
            state_path.with_extension("part").to_str().unwrap(),
            get_decompressed_state_size(chain),
        )?;

        fs::rename(state_path.with_extension("part"), &state_path).await?;

        if !file_has_size(&state_path, get_decompressed_state_size(chain)).await? {
            bail!("decompressed state size mismatch");
        }
        println!("✅  state decompressed");

        // Clean up compressed file
        fs::remove_file(&compressed_path).await.ok();
    }

    // Download header if needed
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
        println!("✅  header downloaded");
    } else {
        println!("✅  header already present");
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
