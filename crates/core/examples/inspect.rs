//! Inspect one or more Kanata traces with the same engine used by the viewers.
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use xonata_core::storage::NativeBacking;
use xonata_core::{Engine, Event, Request, SearchQuery};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.is_empty() {
        return Err("usage: cargo run -p xonata-core --example inspect -- TRACE...".into());
    }
    futures_lite::future::block_on(async {
        let mut engine = Engine::new(Box::new(NativeBacking::new()?));
        for (index, path) in paths.iter().enumerate() {
            let id = u32::try_from(index + 1)?;
            let path = Path::new(path);
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            engine.open(id, name).await?;
            let mut file = BufReader::new(File::open(path)?);
            let mut buffer = vec![0_u8; 256 * 1024];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                engine.feed(id, &buffer[..count]).await?;
            }
            let Event::Info { info } = engine.finish(id).await? else {
                unreachable!()
            };
            println!(
                "{}: {} operations, {} flushed, cycles {}–{}, {} warnings, {} bytes",
                path.display(),
                info.count,
                info.flushed,
                info.first_cycle,
                info.last_cycle,
                info.warning_count,
                info.bytes
            );
            for warning in &info.warnings {
                println!("  {warning}");
            }
            let view = engine
                .request(Request::View {
                    trace: id,
                    generation: 1,
                    start: 0,
                    stride: 1,
                    count: 16,
                    hide_flushed: false,
                })
                .await?;
            if let Some(Event::View { rows, .. }) = view {
                println!("  first viewport: {} rows", rows.len());
                println!(
                    "  first stage cycle: {:?}",
                    rows.iter()
                        .flat_map(|row| row.op.stages.iter().map(|stage| stage.start))
                        .min()
                );
            }
            let query = SearchQuery {
                text: "add".into(),
                ..Default::default()
            };
            engine
                .request(Request::Search {
                    trace: id,
                    generation: 1,
                    query: Box::new(query),
                })
                .await?;
            let mut total = 0;
            while engine.has_search_work() {
                for event in engine.tick().await? {
                    if let Event::SearchProgress { total: count, .. } = event {
                        total = count;
                    }
                }
            }
            println!("  search 'add': {total} matches");
            if total > 0 {
                let results = engine
                    .request(Request::Results {
                        trace: id,
                        generation: 1,
                        start: 0,
                        count: 3,
                    })
                    .await?;
                if let Some(Event::Results { hits, .. }) = results {
                    println!("  fetched {} matching snippets", hits.len());
                }
            }
        }
        println!("shared cache: {:?}", engine.metrics());
        Ok(())
    })
}
