//! Prints an address's UTxOs, then every change to them as blocks arrive.
//!
//! ```text
//! cargo run --example watch -- <socket> <mainnet|preprod|preview|MAGIC> <address>
//! ```

use efreet::{Point, Utxo, Watcher};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [socket, network, address] = args.as_slice() else {
        eprintln!("usage: watch <socket> <mainnet|preprod|preview|MAGIC> <address>");
        std::process::exit(2);
    };

    let magic = match network.as_str() {
        "mainnet" => efreet::MAINNET_MAGIC,
        "preprod" => efreet::PREPROD_MAGIC,
        "preview" => efreet::PREVIEW_MAGIC,
        magic => magic.parse()?,
    };

    let mut watcher = Watcher::connect(socket, magic, address).await?;

    println!(
        "at {}: {} utxos, {} lovelace",
        fmt_point(watcher.point()),
        watcher.utxos().count(),
        watcher.lovelace(),
    );
    for utxo in watcher.utxos() {
        println!("  {}", fmt_utxo(utxo));
    }

    loop {
        let update = tokio::select! {
            update = watcher.next() => update?,
            _ = tokio::signal::ctrl_c() => break,
        };

        let kind = if update.rollback {
            "rollback to"
        } else {
            "block"
        };
        println!("{kind} {}", fmt_point(&update.point));
        for utxo in &update.added {
            println!("  + {}", fmt_utxo(utxo));
        }
        for utxo in &update.removed {
            println!("  - {}", fmt_utxo(utxo));
        }
        println!("  = {} lovelace", watcher.lovelace());
    }

    watcher.close().await;
    Ok(())
}

fn fmt_point(point: &Point) -> String {
    match point {
        Point::Origin => "origin".into(),
        Point::Specific(slot, hash) => format!("slot {slot} ({})", hex::encode(hash)),
    }
}

fn fmt_utxo(utxo: &Utxo) -> String {
    let assets = match utxo.assets.len() {
        0 => String::new(),
        n => format!(" + {n} asset(s)"),
    };
    format!("{} {} lovelace{assets}", utxo.output_ref, utxo.lovelace)
}
