//! Optional regression check using the supplied simulator trace.
use futures_lite::future::block_on;
use xonata_core::{Engine, Event, Request, SearchQuery, storage::NativeBacking};

#[test]
#[ignore = "requires the supplied drive-download trace directory"]
fn vfirst_search_should_load_every_match_during_and_after_scanning() {
    block_on(async {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../drive-download-20261001T142849Z-1-001/scr_base_lite_kanata_core0(3).log");
        let bytes = std::fs::read(path).unwrap();
        let mut engine = Engine::new(Box::new(NativeBacking::new().unwrap()));
        engine.open(1, "vfirst.log".into()).await.unwrap();
        for chunk in bytes.chunks(64 * 1024) {
            engine.feed(1, chunk).await.unwrap();
        }
        engine.finish(1).await.unwrap();
        engine
            .request(Request::Search {
                trace: 1,
                generation: 1,
                query: Box::new(SearchQuery {
                    text: "vfirst".into(),
                    ..Default::default()
                }),
            })
            .await
            .unwrap();
        let mut total = 0;
        let mut batches = 0;
        while engine.has_search_work() {
            for event in engine.tick().await.unwrap() {
                if let Event::SearchProgress { total: found, .. } = event {
                    total = found;
                }
            }
            for start in (0..total).step_by(64) {
                let Some(Event::Results { hits, .. }) = engine
                    .request(Request::Results {
                        trace: 1,
                        generation: 1,
                        start,
                        count: 64,
                    })
                    .await
                    .unwrap()
                else {
                    panic!("expected results");
                };
                assert_eq!(hits.len() as u64, (total - start).min(64));
                assert!(hits.iter().all(|hit| {
                    hit.snippets
                        .iter()
                        .any(|snippet| snippet.to_lowercase().contains("vfirst"))
                }));
            }
            batches += 1;
        }
        assert!(total > 1);
        println!("Verified all {total} vfirst matches over {batches} progressive search batches");
        engine
            .request(Request::Overview {
                trace: 1,
                generation: 1,
                width: 256,
                height: 1024,
            })
            .await
            .unwrap();
        let mut overview = None;
        while engine.has_search_work() {
            for event in engine.tick().await.unwrap() {
                if let Event::Overview { raster, .. } = event {
                    overview = Some(raster);
                }
            }
        }
        let raster = overview.unwrap();
        assert_eq!(
            (raster.width, raster.height, raster.pixels.len()),
            (256, 1024, 262_144)
        );
        assert!(
            raster.pixels[raster.pixels.len() - 256..]
                .iter()
                .any(|pixel| *pixel != 0)
        );
        println!("Whole-trace overview includes the final pipeline rows in 256 KiB");
    });
}
