    use super::*;
use crate::order::SourceFrame;
use crate::proc::{self, ActiveNative};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

    use std::os::unix::fs::PermissionsExt;
    fn work(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rawtl-batch-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn fake(dir: &Path, mode: &str) -> PathBuf {
        let tool = dir.join(format!("native-{mode}"));
        let script=r#"#!/usr/bin/env python3
import sys,pathlib,urllib.parse,hashlib,socket,signal
mode=MODE
request=pathlib.Path(sys.argv[2]); root=request.parent
fields=dict((k,urllib.parse.unquote(v)) for k,v in (line.split('=',1) for line in request.read_text().splitlines()[1:]))
def encode(x): return urllib.parse.quote(str(x),safe='/ :,._-')
n=int(fields['count']); result='rawtl-prepare-result/1\ncount=%d\n'%n
for i in range(n):
 src=fields['item%d_source'%i]; ctx=pathlib.Path(fields['item%d_context'%i]); id=fields['item%d_id'%i]; h=hashlib.sha256(pathlib.Path(src).read_bytes()).hexdigest()
 ctx.write_text('rawtl-prepared-frame/2\n'+''.join(k+'='+encode(v)+'\n' for k,v in [('source',src),('source_sha256',h),('route','linearraw-sdk'),('prepared_dng',src),('prepared_sha256',h),('generated',0),('main_ifd_width',64),('main_ifd_height',64),('as_shot_neutral','0.5,1,0.8'),('has_capture_time',0),('converter_used',0)]))
 if mode=='missing' and i==n-1: continue
 if mode=='extra' and i==n-1: id='999'
 if mode=='duplicate': id='0'
 if mode=='source' and i==n-1: src=fields['item0_source']
 result+=''.join('item%d_%s=%s\n'%(i,k,encode(v)) for k,v in [('id',id),('source',src),('source_sha256',h),('context',ctx)])
if mode.startswith('late:'):
 signal.signal(signal.SIGTERM,signal.SIG_IGN)
 s=socket.socket();s.connect(('127.0.0.1',int(mode.split(':')[1])));s.sendall(b'ready');s.recv(1)
(root/'prepare.result').write_text(result)
"#.replace("MODE",&format!("{mode:?}"));
        fs::write(&tool, script).unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        tool
    }
    fn sources(dir: &Path) -> Vec<SourceFrame> {
        (0..2)
            .map(|i| {
                let path = dir.join(format!("source{i}.dng"));
                fs::write(&path, b"source").unwrap();
                SourceFrame {
                    path,
                    name: format!("source{i}.dng"),
                    key: Vec::new(),
                }
            })
            .collect()
    }
    #[test]
    fn batch_adapter_checks_complete_result_identity_sets() {
        let dir = work("sets");
        let sources = sources(&dir);
        let items: Vec<_> = sources
            .iter()
            .enumerate()
            .map(|(frame_id, source)| PrepareBatchItem { frame_id, source })
            .collect();
        for mode in ["ok", "missing", "extra", "duplicate", "source"] {
            let tool = fake(&dir, mode);
            let scratch = dir.join(mode);
            fs::create_dir(&scratch).unwrap();
            let result = prepare_batch_active(
                &tool,
                &items,
                &PrepareBatchSettings {
                    lens: Default::default(),
                    scratch_dir: &scratch,
                    converter: None,
                    converter_timeout: None,
                },
                Default::default(),
            );
            if mode == "ok" {
                assert_eq!(
                    result
                        .unwrap()
                        .iter()
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>(),
                    [0, 1]
                );
            } else {
                assert!(result.is_err(), "accepted {mode}");
            }
        }
        let scratch = dir.join("duplicate-request");
        fs::create_dir(&scratch).unwrap();
        let duplicate = [
            PrepareBatchItem {
                frame_id: 1,
                source: &sources[0],
            },
            PrepareBatchItem {
                frame_id: 1,
                source: &sources[1],
            },
        ];
        assert!(prepare_batch_active(
            &fake(&dir, "ok"),
            &duplicate,
            &PrepareBatchSettings {
                lens: Default::default(),
                scratch_dir: &scratch,
                converter: None,
                converter_timeout: None
            },
            Default::default()
        )
        .is_err());
    }
    #[test]
    fn batch_adapter_rejects_late_success_after_cancel() {
        use std::io::Write;
        let dir = work("late");
        let sources = sources(&dir);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let tool = fake(
            &dir,
            &format!("late:{}", listener.local_addr().unwrap().port()),
        );
        let scratch = dir.join("scratch");
        fs::create_dir(&scratch).unwrap();
        let active: ActiveNative = Default::default();
        let worker_active = active.clone();
        let worker = std::thread::spawn(move || {
            let items: Vec<_> = sources
                .iter()
                .enumerate()
                .map(|(frame_id, source)| PrepareBatchItem { frame_id, source })
                .collect();
            prepare_batch_active(
                &tool,
                &items,
                &PrepareBatchSettings {
                    lens: Default::default(),
                    scratch_dir: &scratch,
                    converter: None,
                    converter_timeout: None,
                },
                worker_active,
            )
        });
        let start = Instant::now();
        let mut stream = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(start.elapsed() < Duration::from_secs(10));
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut ready = [0; 5];
        stream.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready");
        proc::cancel(&active);
        stream.write_all(b"x").unwrap();
        assert!(worker
            .join()
            .unwrap()
            .unwrap_err()
            .message
            .contains("cancelled"));
        assert!(active.lock().unwrap().children.is_empty());
    }
    /// Executed with RAWTL_BATCH_REAL_WORK and RAWTL_NATIVE_TOOL in S01. With no
    /// external materials configured, ordinary cargo tests exercise mocks only.
    #[test]
    fn batch_real_native_roundtrip_and_single_frame_equivalence() {
        let Some(work) = std::env::var_os("RAWTL_BATCH_REAL_WORK") else {
            eprintln!("real batch materials not configured");
            return;
        };
        let dir = PathBuf::from(work);
        fs::create_dir_all(&dir).unwrap();
        let tool = PathBuf::from(std::env::var_os("RAWTL_NATIVE_TOOL").expect("RAWTL_NATIVE_TOOL"));
        let sources: Vec<_> = std::env::var("RAWTL_REAL_CR3")
            .expect("RAWTL_REAL_CR3")
            .split(',')
            .map(|p| SourceFrame {
                path: PathBuf::from(p),
                name: Path::new(p).file_name().unwrap().to_str().unwrap().into(),
                key: Vec::new(),
            })
            .collect();
        let batch_dir = dir.join("batch");
        fs::create_dir(&batch_dir).unwrap();
        let items: Vec<_> = sources
            .iter()
            .enumerate()
            .map(|(frame_id, source)| PrepareBatchItem { frame_id, source })
            .collect();
        let batch = prepare_batch_active(
            &tool,
            &items,
            &PrepareBatchSettings {
                lens: Default::default(),
                scratch_dir: &batch_dir,
                converter: None,
                converter_timeout: Some(120),
            },
            Default::default(),
        )
        .expect("real batch");
        for ((id, frame), source) in batch.iter().zip(&sources) {
            let single_dir = dir.join(format!("single-{id}"));
            fs::create_dir(&single_dir).unwrap();
            let single = prepare(
                &tool,
                source,
                &PrepareSettings {
                    lens: Default::default(),
                    scratch_dir: &single_dir,
                    context_path: &single_dir.join("frame.context"),
                    log: &single_dir.join("native.log"),
                    converter: None,
                    converter_timeout: Some(120),
                },
            )
            .expect("real single");
            assert_eq!(frame.source_sha256, single.source_sha256);
            assert_eq!(frame.as_shot_neutral, single.as_shot_neutral);
            assert_eq!(frame.capture_time, single.capture_time);
            for method in [
                crate::color_profile::CameraRecipe::LegacyInverseCm,
                crate::color_profile::CameraRecipe::CanonCr3,
            ] {
                if method == crate::color_profile::CameraRecipe::CanonCr3
                    && frame.route != PreparedRoute::VendorRaw
                {
                    continue;
                }
                let settings = PreparedSettings {
                    lens: Default::default(),
                    camera_recipe: method,
                };
                let a = analyze_prepared(
                    &tool,
                    frame,
                    None,
                    Some(0.),
                    None,
                    &settings,
                    &dir.join(format!("analysis-batch-{id}-{method:?}.log")),
                )
                .unwrap();
                let b = analyze_prepared(
                    &tool,
                    &single,
                    None,
                    Some(0.),
                    None,
                    &settings,
                    &dir.join(format!("analysis-single-{id}-{method:?}.log")),
                )
                .unwrap();
                assert_eq!(analysis_bytes(&a), analysis_bytes(&b));
                let mut digests = Vec::new();
                for (label, f) in [("batch", frame), ("single", &single)] {
                    develop_prepared(
                        &tool,
                        f,
                        None,
                        Some(0.25),
                        &settings,
                        &dir.join(format!("develop-{label}-{id}-{method:?}.log")),
                        &mut |info, native| {
                            digests.push(crate::digest::sha256(
                                &native.pixels(info.width, info.height)?,
                            ));
                            Ok(())
                        },
                    )
                    .unwrap();
                }
                assert_eq!(digests[0], digests[1]);
                eprintln!(
                    "frame={id} method={method:?} source={} wb={:?} time={:?} pixel_sha256={}",
                    frame.source_sha256, frame.as_shot_neutral, frame.capture_time, digests[0]
                );
            }
        }
        let context = &batch.iter().find(|(_, f)| f.generated).unwrap().1.context;
        let valid = fs::read_to_string(context).unwrap();
        for text in [
            valid.replace("rawtl-prepared-frame/2", "rawtl-prepared-frame/1"),
            valid[..valid.len() - 10].to_string(),
            valid.replace("converter_record_sha256=", "converter_record_sha256=0"),
        ] {
            let path = dir.join("bad.context");
            fs::write(&path, text).unwrap();
            assert!(PreparedFrame::load(&path).is_err());
        }
        let parsed = parse_context_fields(valid.as_bytes()).unwrap();
        let record_path = Path::new(field_value(&parsed, "converter_record").unwrap());
        let old_hash = field_value(&parsed, "converter_record_sha256").unwrap();
        let wrong_record = dir.join("wrong.record");
        let record = fs::read_to_string(record_path)
            .unwrap()
            .replace("item0_source=", "item0_source=/wrong-source");
        fs::write(&wrong_record, &record).unwrap();
        let wrong_context = dir.join("wrong-record.context");
        fs::write(
            &wrong_context,
            valid
                .replace(
                    record_path.to_str().unwrap(),
                    wrong_record.to_str().unwrap(),
                )
                .replace(old_hash, &crate::digest::sha256(record.as_bytes())),
        )
        .unwrap();
        assert!(
            PreparedFrame::load(&wrong_context).is_err(),
            "wrong record with matching byte hash"
        );
        if let Some((_, deleted)) = batch.iter().find(|(_, f)| f.generated) {
            fs::remove_file(&deleted.dng).unwrap();
            let f = batch
                .iter()
                .find(|(_, f)| f.dng != deleted.dng)
                .map(|(_, f)| PreparedFrame::load(&f.context).unwrap())
                .unwrap();
            analyze_prepared(
                &tool,
                &f,
                None,
                Some(0.),
                None,
                &PreparedSettings {
                    lens: Default::default(),
                    camera_recipe: crate::color_profile::CameraRecipe::LegacyInverseCm,
                },
                &dir.join("after-delete.log"),
            )
            .unwrap();
        }
    }

    #[test]
    fn native_batch_context_fixtures_load_and_consume() {
        let Some(root) = std::env::var_os("RAWTL_BATCH_FIXTURE_ROOT") else {
            eprintln!("native batch context fixtures not configured");
            return;
        };
        let root = PathBuf::from(root);
        let tool = PathBuf::from(std::env::var_os("RAWTL_NATIVE_TOOL").expect("RAWTL_NATIVE_TOOL"));
        let settings = PreparedSettings {
            lens: Default::default(),
            camera_recipe: crate::color_profile::CameraRecipe::LegacyInverseCm,
        };
        for name in ["context-nul.ctx", "record-nul.ctx"] {
            let error = PreparedFrame::load(&root.join(name)).expect_err("forged NUL must be rejected");
            assert!(
                error.message.contains("prepared document contains NUL"),
                "{}",
                error.message
            );
        }
        for (i, name) in [
            "many/0.ctx",
            "noisy/0.ctx",
            "mapping/5.ctx",
            "all-direct/0.ctx",
            "mixed/2.ctx",
            "nul/0.ctx",
        ]
        .iter()
        .enumerate()
        {
            let frame = PreparedFrame::load(&root.join(name)).expect("actual native context");
            analyze_prepared(
                &tool,
                &frame,
                None,
                Some(0.),
                None,
                &settings,
                &root.join(format!("rust-fixture-{i}-analysis.log")),
            )
            .unwrap();
            develop_prepared(
                &tool,
                &frame,
                None,
                Some(0.),
                &settings,
                &root.join(format!("rust-fixture-{i}-develop.log")),
                &mut |info, stream| {
                    assert_eq!(
                        stream.pixels(info.width, info.height)?.len(),
                        info.width as usize * info.height as usize * 12
                    );
                    Ok(())
                },
            )
            .unwrap();
        }
        for name in [
            "wrong-record.ctx",
            "record-version.ctx",
            "record-truncated.ctx",
        ] {
            assert!(
                PreparedFrame::load(&root.join(name)).is_err(),
                "accepted {name}"
            );
        }
        let frame = PreparedFrame::load(&root.join("many/0.ctx")).unwrap();
        let fields = parse_context_fields(&fs::read(&frame.context).unwrap()).unwrap();
        let record = read_document(
            Path::new(field_value(&fields, "converter_record").unwrap()),
            1024 * 1024,
        )
        .unwrap();
        let fields = parse_document_fields(&record, "rawtl-conversion/1", 1024 * 1024).unwrap();
        assert_eq!(field_value(&fields, "argc").unwrap(), "26");
        let frame = PreparedFrame::load(&root.join("nul/0.ctx")).unwrap();
        let fields = parse_context_fields(&fs::read(&frame.context).unwrap()).unwrap();
        let record = read_document(
            Path::new(field_value(&fields, "converter_record").unwrap()),
            1024 * 1024,
        )
        .unwrap();
        let fields = parse_document_fields(&record, "rawtl-conversion/1", 1024 * 1024).unwrap();
        assert_eq!(
            field_value(&fields, "transcript").unwrap(),
            "warning before?after\n"
        );
    }
