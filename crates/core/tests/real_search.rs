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

#[test]
#[ignore = "requires the supplied drive-download trace directory"]
fn interval_filters_should_match_independently_calculated_real_phase_pairs() {
    block_on(async {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../drive-download-20261001T142849Z-1-001/scr_base_lite_kanata_core0(3).log");
        let mut engine = Engine::new(Box::new(NativeBacking::new().unwrap()));
        engine.open(1, "real-filter.log".into()).await.unwrap();
        for chunk in std::fs::read(path).unwrap().chunks(64 * 1024) {
            engine.feed(1, chunk).await.unwrap();
        }
        engine.finish(1).await.unwrap();
        // Reference pairs were computed from raw I/L/S/E/R records, independently
        // of FilterQuery and the worker matcher. In this file these are overlaps.
        let expected = [
            (5497, 5499, -11),
            (5501, 5503, -15),
            (5631, 5633, -12),
            (5635, 5637, -16),
            (5841, 5843, -12),
            (5845, 5847, -16),
            (6051, 6053, -11),
            (6055, 6057, -15),
            (25653, 25655, -12),
            (25657, 25659, -16),
            (26170, 26172, -14),
            (26174, 26176, -13),
        ];
        let query = "from=E & phase_pos=END & instr=vfirst & meta=\"FREE RQU *=36\" -> to=E & phase_pos=START & instr=vmsne; label=real_pairs";
        engine
            .request(Request::Filter {
                trace: 1,
                generation: 1,
                text: query.into(),
                skip: 0,
            })
            .await
            .unwrap();
        let mut total = 0;
        while engine.has_search_work() {
            for event in engine.tick().await.unwrap() {
                match event {
                    Event::FilterProgress { total: n, .. } => total = n,
                    Event::FilterError { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
        }
        assert_eq!(total, expected.len() as u64);
        let Some(Event::FilterResults { hits, .. }) = engine
            .request(Request::FilterResults {
                trace: 1,
                generation: 1,
                start: 0,
                count: 128,
            })
            .await
            .unwrap()
        else {
            panic!("expected results")
        };
        assert_eq!(
            hits.iter()
                .map(|h| (h.source.op, h.target.as_ref().unwrap().op, h.elapsed))
                .collect::<Vec<_>>(),
            expected
        );
        engine
            .request(Request::Filter {
                trace: 1,
                generation: 2,
                text: query.replace("=E", "=vE"),
                skip: 0,
            })
            .await
            .unwrap();
        while engine.has_search_work() {
            for event in engine.tick().await.unwrap() {
                if let Event::FilterProgress { total, .. } = event {
                    assert_eq!(total, 0);
                }
            }
        }
        for (generation, scope, count) in [(3, "rows", 12), (4, "all", 11)] {
            // The END of op 5497 is 204913, strictly inside 5501 -> 5503's
            // [204902,204917] interval, but its row precedes both endpoints.
            let excluded = serde_json::to_string("phase=E & op=5497 & phase_pos=END").unwrap();
            engine
                .request(Request::Filter {
                    trace: 1,
                    generation,
                    text: format!("{query}; exclude={excluded}; exclude_scope={scope}"),
                    skip: 0,
                })
                .await
                .unwrap();
            let mut total = 0;
            while engine.has_search_work() {
                for event in engine.tick().await.unwrap() {
                    match event {
                        Event::FilterProgress { total: found, .. } => total = found,
                        Event::FilterError { message, .. } => panic!("{message}"),
                        _ => {}
                    }
                }
            }
            assert_eq!(total, count);
            let Some(Event::FilterResults { hits, .. }) = engine
                .request(Request::FilterResults {
                    trace: 1,
                    generation,
                    start: 0,
                    count: 128,
                })
                .await
                .unwrap()
            else {
                panic!("missing real exclusions");
            };
            let expected: Vec<_> = expected
                .iter()
                .copied()
                .filter(|pair| scope == "rows" || pair.0 != 5501)
                .collect();
            assert_eq!(
                hits.iter()
                    .map(|h| (h.source.op, h.target.as_ref().unwrap().op, h.elapsed))
                    .collect::<Vec<_>>(),
                expected
            );
        }
        println!("Verified all 12 real interval pairs and direct-row vE mismatch");
    });
}
