# What are the downloaded files?

Reth for Gnosis currently downloads state files for starting sync, because it cannot sync pre-merge blocks. Until snap-sync is introduced, these files are necessary to initialize a Reth node for Gnosis.

There are 2 files each, for Gnosis Chain and Chiado:
- The header file (in RLP format) at the import height
- The state file (in zstd compressed format) at the import height

The import heights are:
- Gnosis Chain: EL Block 26_478_650
    - Hash: 0xa133198478cb01b4585604d07f584633f1f147103b49672d2bd87a5a3ba2c06e
- Chiado: EL Block 700_000
    - Hash: 0xcdc424294195555e53949b6043339a49b049b48caa8d85bc7d5f5d12a85964b6

## How to recreate the state files?

We use geth's state dump tool to create these files. You need to run geth for Gnosis Chain and Chiado respectively using the flag `--cache.preimages --syncmode=full` until the block :
```sh
geth --gnosis --cache.preimages --syncmode=full --synctarget 0xa133198478cb01b4585604d07f584633f1f147103b49672d2bd87a5a3ba2c06e
```
```sh
geth --chiado --cache.preimages --syncmode=full --synctarget 0xcdc424294195555e53949b6043339a49b049b48caa8d85bc7d5f5d12a85964b6
```

Once the node has synced to the target block, you can do a state dump using the command:
```sh
geth --nodiscover dump --iterative <height> > state.jsonl
```

Next, we compress the state file using zstd. We do this in rust using these functions:

```rust
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::File;
use std::fs::metadata;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use zstd::stream::read::Decoder;
use zstd::stream::write::Encoder;

fn compress_zstd(input: &str, output: &str, level: i32) -> std::io::Result<()> {
    let input_file = File::open(input)?;
    let file_size = metadata(input)?.len(); // Get file size in bytes
    let reader = BufReader::new(&input_file);
    let writer = BufWriter::new(File::create(output)?);
    let mut encoder = Encoder::new(writer, level)?;

    let pb = ProgressBar::new(file_size);
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("=>-"));

    for line in reader.lines() {
        let line = line?;
        writeln!(encoder, "{}", line)?;
        pb.inc(line.len() as u64 + 1); // Include newline
    }

    encoder.finish()?;
    pb.finish_with_message("Compression complete");
    Ok(())
}

fn compress() {
    let file = "gnosis/state_at_26478650.jsonl";
    let level = 15; // Compression level
    // format output file like "compressed_{level}.jsonl.zst"
    let output = format!("compressed_{}.jsonl.zst", level);

    if let Err(e) = compress_zstd(file, output.as_str(), level) {
        eprintln!("Error compressing file: {}", e);
    } else {
        println!("File compressed successfully to {}", output);
    }
}
```
