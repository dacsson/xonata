use futures_lite::future::block_on;
use xonata_core::storage::{NativeBacking, PageStore};
use xonata_core::{Engine, Event, Request, SearchQuery};

const FIXTURE: &[u8] = b"Kanata\t0004\nC=\t100\nI\t0\t100\t0\nL\t0\t0\tproducer\\nname\nL\t0\t1\tproducer detail\nS\t0\t0\tF\nC\t1\nS\t0\t0\tX\nL\t0\t2\texecute\\nlabel\nC\t2\nS\t0\t0\tRt\nI\t1\t101\t1\nL\t1\t0\tconsumer\nS\t1\t0\tF\nS\t1\t1\tstl\nW\t1\t0\t7\nC\t1\nE\t1\t0\tF\nR\t0\t0\t0\nC\t1\nR\t1\t1\t1\nL\t0\t1\t; post-retire detail\n";

fn engine() -> Engine {
    Engine::new(Box::new(NativeBacking::new().unwrap()))
}

#[test]
fn kanata_fixture_should_preserve_stages_metadata_dependencies_and_flush() {
    block_on(async {
        let mut engine = engine();
        engine.open(1, "basic.kanata".into()).await.unwrap();
        for chunk in FIXTURE.chunks(7) {
            engine.feed(1, chunk).await.unwrap();
        }
        let info = engine.finish(1).await.unwrap();
        let Event::Info { info } = info else {
            panic!("expected info")
        };
        assert_eq!(
            (info.count, info.flushed, info.first_cycle, info.last_cycle),
            (2, 1, 100, 105)
        );
        assert_eq!(info.warning_count, 0);
        let event = engine
            .request(Request::View {
                trace: 1,
                generation: 1,
                start: 0,
                stride: 1,
                count: 10,
                hide_flushed: false,
            })
            .await
            .unwrap()
            .unwrap();
        let Event::View { rows, .. } = event else {
            panic!("expected rows")
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].op.label, "producer\nname");
        assert!(rows[0].op.detail.contains("post-retire"));
        assert_eq!(rows[0].op.stages.len(), 3);
        assert_eq!(rows[0].op.stages[1].labels, "execute\nlabel");
        assert_eq!(rows[1].op.dependencies[0].producer, 0);
        assert!(rows[1].op.flushed);
    });
}

#[test]
fn simulator_resource_snapshots_should_not_be_parser_warnings() {
    block_on(async {
        let mut engine = engine();
        engine.open(1, "resources.kanata".into()).await.unwrap();
        let log = b"Kanata\t0004\ncore=SCR9:width=4\nI\t1\t0\t0\nRQU=15:RSQ=14\nR\t1\t1\t0\nL\t1\t0\tadd x1,x2,x3\n";
        engine.feed(1, log).await.unwrap();
        let Event::Info { info } = engine.finish(1).await.unwrap() else {
            panic!("expected info")
        };
        assert_eq!(info.count, 1);
        assert_eq!(info.warning_count, 0);
    });
}

#[test]
fn search_should_return_every_matching_instruction_and_filter_status() {
    block_on(async {
        let mut engine = engine();
        engine.open(1, "basic.kanata".into()).await.unwrap();
        engine.feed(1, FIXTURE).await.unwrap();
        engine.finish(1).await.unwrap();
        let query = SearchQuery {
            text: "producer|consumer".into(),
            regex: true,
            ..Default::default()
        };
        engine
            .request(Request::Search {
                trace: 1,
                generation: 3,
                query: Box::new(query),
            })
            .await
            .unwrap();
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        let Event::Results { hits, .. } = engine
            .request(Request::Results {
                trace: 1,
                generation: 3,
                start: 0,
                count: 10,
            })
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected results")
        };
        assert_eq!(
            hits.iter().map(|hit| hit.op.id).collect::<Vec<_>>(),
            vec![0, 1]
        );
        let query = SearchQuery {
            status: "flushed".into(),
            ..Default::default()
        };
        engine
            .request(Request::Search {
                trace: 1,
                generation: 4,
                query: Box::new(query),
            })
            .await
            .unwrap();
        while engine.has_search_work() {
            engine.tick().await.unwrap();
        }
        let Event::Results { hits, .. } = engine
            .request(Request::Results {
                trace: 1,
                generation: 4,
                start: 0,
                count: 10,
            })
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected results")
        };
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].op.id, 1);
    });
}

#[test]
fn cache_eviction_should_round_trip_instruction_pages() {
    block_on(async {
        let mut store = PageStore::new(Box::new(NativeBacking::new().unwrap()), 8 * 1024);
        for id in [0, 256, 512] {
            store
                .set(
                    1,
                    xonata_core::Operation {
                        id,
                        gid: id,
                        tid: 0,
                        rid: Some(id),
                        fetch: id,
                        end: Some(id + 1),
                        flushed: false,
                        incomplete: false,
                        line: 1,
                        label: "abc".into(),
                        detail: "large".repeat(100),
                        stages: Vec::new(),
                        dependencies: Vec::new(),
                    },
                )
                .await
                .unwrap();
        }
        assert_eq!(store.get(1, 0).await.unwrap().unwrap().label, "abc");
        assert_eq!(store.visible_ids(1, 0, 1, 3, false).len(), 3);
    });
}

#[test]
fn incomplete_trace_should_keep_unretired_instruction() {
    block_on(async {
        let mut engine = engine();
        engine.open(1, "incomplete.kanata".into()).await.unwrap();
        engine
            .feed(1, b"Kanata\t0004\nI\t8\t9\t0\nS\t8\t0\tF\n")
            .await
            .unwrap();
        engine.finish(1).await.unwrap();
        let Event::View { rows, .. } = engine
            .request(Request::View {
                trace: 1,
                generation: 1,
                start: 0,
                stride: 1,
                count: 1,
                hide_flushed: false,
            })
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected rows")
        };
        assert!(rows[0].op.incomplete);
    });
}

#[test]
fn gzip_and_zstd_should_decode_across_arbitrary_chunk_boundaries() {
    block_on(async {
        for (name, bytes) in [
            (
                "trace.gz",
                &include_bytes!("fixtures/compressed.kanata.gz")[..],
            ),
            (
                "trace.zst",
                &include_bytes!("fixtures/compressed.kanata.zst")[..],
            ),
            (
                "trace.zst",
                &include_bytes!("fixtures/compressed-no-check.zst")[..],
            ),
        ] {
            for width in [1, 7, 64, 256] {
                let mut engine = engine();
                engine.open(1, name.into()).await.unwrap();
                for chunk in bytes.chunks(width) {
                    engine.feed(1, chunk).await.unwrap();
                }
                let info = engine.finish(1).await.unwrap();
                let Event::Info { info } = info else {
                    panic!("expected info")
                };
                assert_eq!(
                    (info.count, info.first_cycle, info.last_cycle),
                    (1, 7, 9),
                    "{name} chunk {width}"
                );
            }
        }
    });
}

#[test]
fn concatenated_zstd_frames_should_decode_without_retaining_the_whole_file() {
    let frame = include_bytes!("fixtures/compressed.kanata.zst");
    let mut compressed = Vec::from(&frame[..]);
    compressed.extend_from_slice(frame);
    let mut decoder = xonata_core::compression::Decoder::from_name("two.zst");
    let mut output = Vec::new();
    for chunk in compressed.chunks(7) {
        for decoded in decoder.feed(chunk).unwrap() {
            output.extend_from_slice(&decoded);
        }
    }
    output.extend(decoder.finish().unwrap());
    let expected = include_bytes!("fixtures/compressed.kanata");
    assert_eq!(output, [expected.as_slice(), expected.as_slice()].concat());
}

#[test]
fn overview_should_cover_all_rows_with_bounded_storage_and_support_row_jumps() {
    block_on(async {
        let mut engine = engine();
        engine.open(1, "overview.kanata".into()).await.unwrap();
        engine.feed(1, FIXTURE).await.unwrap();
        engine.finish(1).await.unwrap();
        engine
            .request(Request::Overview {
                trace: 1,
                generation: 2,
                width: u16::MAX,
                height: u16::MAX,
            })
            .await
            .unwrap();
        let mut raster = None;
        while engine.has_search_work() {
            for event in engine.tick().await.unwrap() {
                if let Event::Overview {
                    generation,
                    raster: image,
                    ..
                } = event
                {
                    assert_eq!(generation, 2);
                    raster = Some(image);
                }
            }
        }
        let raster = raster.unwrap();
        assert_eq!(
            (raster.width, raster.height, raster.pixels.len()),
            (256, 2, 512)
        );
        assert!(raster.pixels[..256].iter().any(|pixel| *pixel != 0));
        assert!(raster.pixels[256..].iter().any(|pixel| *pixel & 128 != 0));
        let Some(Event::Jump { row: Some(row), .. }) = engine
            .request(Request::JumpRow { trace: 1, row: 1 })
            .await
            .unwrap()
        else {
            panic!("expected row jump");
        };
        assert_eq!((row.index, row.op.id), (1, 1));
    });
}

#[test]
fn overview_should_replace_obsolete_jobs_and_handle_empty_traces() {
    block_on(async {
        let mut engine = engine();
        engine.open(1, "empty.kanata".into()).await.unwrap();
        engine.feed(1, b"Kanata\t0004\n").await.unwrap();
        engine.finish(1).await.unwrap();
        for generation in [1, 2] {
            engine
                .request(Request::Overview {
                    trace: 1,
                    generation,
                    width: 0,
                    height: 0,
                })
                .await
                .unwrap();
        }
        let events = engine.tick().await.unwrap();
        assert!(
            matches!(&events[..], [Event::Overview { generation: 2, raster, .. }] if raster.pixels == [0])
        );
        assert!(!engine.has_search_work());
    });
}
