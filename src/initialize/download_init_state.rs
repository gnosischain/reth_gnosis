use anyhow::{bail, Context};
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{BufReader, BufWriter, Read as _, Write as _};
use std::time::Duration;
use std::{fs::File, path::Path};
use tokio::fs::OpenOptions;
use tokio::{fs, io::AsyncWriteExt};
use zstd::Decoder;

const MAX_RETRIES: u32 = 5;
const RETRY_BASE_DELAY_MS: u64 = 2000;

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

// ────────────────────────────────────────────────────────────
// BLAKE3 hashes for integrity verification
// ────────────────────────────────────────────────────────────
fn get_compressed_state_hash(chain: &str) -> &'static str {
    match chain {
        "gnosis" => "68b4c88e3dc02592ec2a1e27cc0556004931a9b4712a4510c8db6aae2f0baaff",
        "chiado" => "e071cb58c5975b66e0db5aea05e4371bbe08b32c41211c5dd9c0c75ca2d592f9",
        _ => unreachable!(),
    }
}

fn get_header_hash(chain: &str) -> &'static str {
    match chain {
        "gnosis" => "9ece59f2a6af4c4af200a0e2ffd19f8dcd9a215b3513171f69a2659651ffa961",
        "chiado" => "b7fa17cc30104ed71791046f894704a60d72150235925240efd538de29d3036b",
        _ => unreachable!(),
    }
}

const HEADER_FILE: &str = "header.rlp";
const STATE_FILE: &str = "state.jsonl";
const COMPRESSED_STATE_FILE: &str = "state.jsonl.zst";

/// Verifies a file's BLAKE3 hash matches the expected value
fn verify_file_hash(path: &Path, expected_hash: &str) -> anyhow::Result<bool> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(false),
    };

    let file_size = file.metadata()?.len();
    let mut reader = BufReader::new(file);
    let mut hasher = blake3::Hasher::new();

    let pb = ProgressBar::new(file_size);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.yellow/blue}] {bytes}/{total_bytes} (verifying)")
            .unwrap()
            .progress_chars("=>-"),
    );

    let mut buffer = [0u8; 65536]; // 64KB buffer for fast hashing
    loop {
        let bytes_read = reader.read(&mut buffer)?;
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
        pb.inc(bytes_read as u64);
    }

    pb.finish_and_clear();

    let hash = hasher.finalize();
    let hash_hex = hash.to_hex();
    Ok(hash_hex.as_str() == expected_hash)
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

/// Downloads a file with progress bar, supporting resumable downloads via HTTP Range.
/// Retries on failure with exponential backoff.
async fn download_file_resumable(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected_size: u64,
) -> anyhow::Result<()> {
    let tmp = dest.with_extension("part");

    let mut current_size: u64 = 0;
    if tmp.exists() {
        current_size = fs::metadata(&tmp).await?.len();
        if current_size >= expected_size {
            fs::rename(&tmp, dest).await?;
            return Ok(());
        }
        if current_size > 0 {
            println!("   resuming from {} / {} bytes", current_size, expected_size);
        }
    }

    let pb = ProgressBar::new(expected_size);
    pb.set_position(current_size);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} {bytes}/{total_bytes} [{bar:40.cyan/blue}] {bytes_per_sec} ETA {eta}",
        )
        .unwrap()
        .progress_chars("#>-"),
    );

    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = RETRY_BASE_DELAY_MS * 2u64.pow(attempt - 1);
            println!("   retry {} in {}s …", attempt + 1, delay / 1000);
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }

        match try_download_chunk(client, url, &tmp, expected_size, &mut current_size, &pb).await {
            Ok(()) => {
                pb.finish();
                fs::rename(&tmp, dest).await?;
                return Ok(());
            }
            Err(e) => {
                last_err = Some(e);
                if current_size > 0 {
                    println!("   download interrupted, {} bytes received", current_size);
                }
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("download failed after {} retries", MAX_RETRIES)))
}

async fn try_download_chunk(
    client: &reqwest::Client,
    url: &str,
    tmp: &Path,
    expected_size: u64,
    current_size: &mut u64,
    pb: &ProgressBar,
) -> anyhow::Result<()> {
    let mut request = client.get(url);

    if *current_size > 0 {
        request = request.header("Range", format!("bytes={}-", *current_size));
    }

    let resp = request.send().await?.error_for_status()?;
    let status = resp.status();

    // If we requested Range but got 200, server ignored Range - do NOT truncate or we lose progress.
    // Retry instead; the partial file stays intact.
    if *current_size > 0 && status.as_u16() != 206 {
        bail!(
            "server returned {} instead of 206 Partial Content (Range not supported), will retry",
            status
        );
    }

    let mut file = if *current_size == 0 {
        fs::File::create(tmp).await?
    } else {
        OpenOptions::new().append(true).open(tmp).await?
    };

    let mut stream = resp.bytes_stream();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result?;
        let len = chunk.len() as u64;
        file.write_all(&chunk).await?;
        *current_size += len;
        pb.set_position(*current_size);
    }
    file.flush().await?;

    if *current_size >= expected_size {
        Ok(())
    } else {
        bail!(
            "incomplete: got {} / {} bytes",
            *current_size,
            expected_size
        )
    }
}

/// Downloads the initial state
pub async fn ensure_state(data_dir: &Path, chain: &str) -> anyhow::Result<()> {
    fs::create_dir_all(data_dir).await?;

    // Remove *.part leftovers except compressed state part (for resumable download)
    let compressed_part = data_dir.join(COMPRESSED_STATE_FILE).with_extension("part");
    let compressed_part_name = compressed_part
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let mut rd = fs::read_dir(data_dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".part") && name != compressed_part_name {
            fs::remove_file(entry.path()).await.ok();
        }
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3600))
        .connect_timeout(Duration::from_secs(30))
        .build()?;

    let state_path = data_dir.join(STATE_FILE);
    let compressed_path = data_dir.join(COMPRESSED_STATE_FILE);
    let header_path = data_dir.join(HEADER_FILE);

    // Check if final decompressed state already exists (size check only)
    if file_has_size(&state_path, get_decompressed_state_size(chain)).await? {
        println!("✅  state already complete");
    } else {
        // Check compressed state with BLAKE3 hash
        let compressed_valid = if compressed_path.exists() {
            println!("🔍  verifying compressed state …");
            verify_file_hash(&compressed_path, get_compressed_state_hash(chain))?
        } else {
            false
        };

        if !compressed_valid {
            if compressed_path.exists() {
                println!("⚠️   hash mismatch, re-downloading …");
                fs::remove_file(&compressed_path).await.ok();
            }

            println!("⬇️   downloading compressed state (resumable) …");
            download_file_resumable(
                &client,
                &get_state_url(chain),
                &compressed_path,
                get_compressed_state_size(chain),
            )
            .await
            .context("failed to download compressed state")?;

            println!("🔍  verifying download …");
            if !verify_file_hash(&compressed_path, get_compressed_state_hash(chain))? {
                fs::remove_file(&compressed_path).await.ok();
                bail!("compressed state hash mismatch - file corrupted");
            }
            println!("✅  compressed state verified");
        } else {
            println!("✅  compressed state verified");
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

    // Check header with BLAKE3 hash
    let header_valid = if header_path.exists() {
        verify_file_hash(&header_path, get_header_hash(chain))?
    } else {
        false
    };

    if !header_valid {
        if header_path.exists() {
            println!("⚠️   header hash mismatch, re-downloading …");
            fs::remove_file(&header_path).await.ok();
        }

        println!("⬇️   downloading header …");
        let bytes = client
            .get(get_header_url(chain))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?;
        fs::write(&header_path, &bytes).await?;

        if !verify_file_hash(&header_path, get_header_hash(chain))? {
            fs::remove_file(&header_path).await.ok();
            bail!("header hash mismatch - file corrupted");
        }
        println!("✅  header verified");
    } else {
        println!("✅  header verified");
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
