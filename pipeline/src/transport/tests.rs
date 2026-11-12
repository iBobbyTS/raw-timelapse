use super::*;

fn descriptor(dtype: DType, shape: [usize; 3]) -> ArrayDescriptor {
    let sample = dtype.byte_width();
    ArrayDescriptor {
        dtype,
        shape: shape.to_vec(),
        byte_strides: vec![shape[1] * shape[2] * sample, shape[2] * sample, sample],
        offset: 0,
        byte_length: shape.iter().product::<usize>() * sample,
        layout: ArrayLayout::HwcInterleaved,
        byte_order: ByteOrder::LittleEndian,
        read_only: true,
    }
}

fn array(dtype: DType, shape: [usize; 3]) -> NdArray {
    let d = descriptor(dtype, shape);
    NdArray::new(vec![7; d.byte_length].into(), d).unwrap()
}

fn metadata(fields: &[(&str, Json)]) -> MetadataDoc {
    MetadataDoc::new(
        fields
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone())),
    )
    .unwrap()
}

fn frame_metadata() -> MetadataDoc {
    metadata(&[
        ("source.width", Json::Int(2)),
        ("source.height", Json::Int(1)),
        ("color.space", Json::Str("ACEScg".into())),
        ("color.transfer", Json::Str("linear".into())),
    ])
}

fn provenance() -> Provenance {
    Provenance::new("rawtl.frame-reader", vec!["source-hash".into()], vec![]).unwrap()
}

fn payload(id: &str) -> Payload {
    match id {
        FRAME_PNG => Payload::DiskArtifact(
            DiskArtifact::new("frame.png", "image/png", Some(42), None).unwrap(),
        ),
        FRAME_METADATA | ANALYSIS_SIDECAR => {
            Payload::MemoryFile(MemoryFile::new(b"{}".to_vec().into(), "application/json").unwrap())
        }
        AP1_F32_FRAME => Payload::NdArray(array(DType::F32, [1, 2, 3])),
        RGB48_FRAME => Payload::NdArray(array(DType::U16, [1, 2, 3])),
        RGB24_FRAME => Payload::NdArray(array(DType::U8, [1, 2, 3])),
        VIDEO_ARTIFACT => Payload::DiskArtifact(
            DiskArtifact::new("linear.mkv", "video/x-matroska", None, None).unwrap(),
        ),
        _ => panic!("unknown test semantic type"),
    }
}

fn semantic_metadata(id: &str) -> MetadataDoc {
    match id {
        FRAME_PNG | RGB48_FRAME | RGB24_FRAME | AP1_F32_FRAME => frame_metadata(),
        FRAME_METADATA => metadata(&[
            ("source.schema_version", Json::Int(1)),
            ("source.width", Json::Int(2)),
            ("source.height", Json::Int(1)),
        ]),
        ANALYSIS_SIDECAR => metadata(&[
            ("source.schema_version", Json::Int(1)),
            ("source.kind", Json::Str("rawtl-analysis-sidecar".into())),
            ("source.sha256", Json::Str("source-hash".into())),
        ]),
        VIDEO_ARTIFACT => metadata(&[
            ("audit.codec", Json::Str("ffv1".into())),
            ("source.frame_count", Json::Int(3)),
        ]),
        _ => panic!("unknown test semantic type"),
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(generated_id());
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_io(error: TransportError, operation: &'static str, path: &Path, kind: io::ErrorKind) {
    match error {
        TransportError::Io {
            operation: actual_operation,
            path: actual_path,
            kind: actual_kind,
            message,
        } => {
            assert_eq!(actual_operation, operation);
            assert_eq!(actual_path, path);
            assert_eq!(actual_kind, kind);
            assert!(!message.is_empty());
        }
        other => panic!("expected {operation} I/O error, got {other:?}"),
    }
}

#[test]
fn disk_artifact_retains_declarations_without_reading_the_path() {
    let scratch = Scratch::new();
    let path = scratch.path("not-yet-produced.png");
    let artifact =
        DiskArtifact::new(&path, "image/png", Some(12), Some("declared-hash".into())).unwrap();
    assert_eq!(artifact.path(), path);
    assert_eq!(artifact.media_type(), "image/png");
    assert_eq!(artifact.size(), Some(12));
    assert_eq!(artifact.hash(), Some("declared-hash"));
    assert!(!path.exists());
    assert_io(
        load(&artifact).unwrap_err(),
        "load",
        &path,
        io::ErrorKind::NotFound,
    );
}

#[test]
fn disk_artifact_rejects_empty_path_and_media_type() {
    assert_eq!(
        DiskArtifact::new("", "image/png", None, None),
        Err(TransportError::EmptyPath)
    );
    assert_eq!(
        DiskArtifact::new("frame.png", " ", None, None),
        Err(TransportError::EmptyMediaType)
    );
}

#[test]
fn memory_file_shares_bytes_and_retains_length_and_content_type() {
    let bytes: Arc<[u8]> = b"encoded png bytes".to_vec().into();
    let file = MemoryFile::new(Arc::clone(&bytes), "image/png").unwrap();
    let copy = file.clone();
    assert!(Arc::ptr_eq(&bytes, &file.shared_bytes()));
    assert!(Arc::ptr_eq(&file.shared_bytes(), &copy.shared_bytes()));
    assert_eq!(copy.len(), 17);
    assert_eq!(copy.bytes(), b"encoded png bytes");
    assert_eq!(copy.content_type(), "image/png");
    assert!(!copy.is_empty());
}

#[test]
fn memory_file_allows_empty_encoded_content_but_rejects_empty_content_type() {
    let empty = MemoryFile::new(Vec::new().into(), "application/json").unwrap();
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
    assert_eq!(
        MemoryFile::new(vec![1].into(), "\t"),
        Err(TransportError::EmptyMediaType)
    );
}

#[test]
fn persist_and_load_are_explicit_and_retain_encoded_bytes_and_media_type() {
    let scratch = Scratch::new();
    let path = scratch.path("frame.png");
    let file = MemoryFile::new(vec![0, 1, 255, 42].into(), "image/png").unwrap();
    assert!(!path.exists());
    let artifact = persist(&file, &path).unwrap();
    assert_eq!(artifact.path(), path);
    assert_eq!(artifact.size(), Some(4));
    assert_eq!(artifact.hash(), None);
    assert_eq!(artifact.media_type(), "image/png");
    assert_eq!(fs::read(&path).unwrap(), file.bytes());
    let loaded = load(&artifact).unwrap();
    assert_eq!(loaded, file);
    fs::remove_file(&path).unwrap();
    assert_eq!(loaded.bytes(), &[0, 1, 255, 42]);
}

#[test]
fn persist_rejects_empty_destination_existing_path_and_missing_parent() {
    let scratch = Scratch::new();
    let file = MemoryFile::new(vec![1, 2].into(), "image/png").unwrap();
    assert_eq!(persist(&file, ""), Err(TransportError::EmptyPath));
    let path = scratch.path("existing.png");
    fs::write(&path, b"original").unwrap();
    assert_io(
        persist(&file, &path).unwrap_err(),
        "persist",
        &path,
        io::ErrorKind::AlreadyExists,
    );
    assert_eq!(fs::read(&path).unwrap(), b"original");
    let missing = scratch.path("missing/frame.png");
    assert_io(
        persist(&file, &missing).unwrap_err(),
        "persist",
        &missing,
        io::ErrorKind::NotFound,
    );
}

#[test]
fn load_rejects_size_mismatch() {
    let scratch = Scratch::new();
    let path = scratch.path("sidecar.json");
    fs::write(&path, b"{}").unwrap();
    let artifact = DiskArtifact::new(path, "application/json", Some(3), None).unwrap();
    assert_eq!(
        load(&artifact),
        Err(TransportError::ArtifactSizeMismatch {
            expected: 3,
            actual: 2
        })
    );
}

#[test]
fn ndarray_accepts_all_supported_dtypes_and_exposes_host_descriptor() {
    for (name, dtype, width) in [
        ("u8", DType::U8, 1),
        ("u16", DType::U16, 2),
        ("i16", DType::I16, 2),
        ("f32", DType::F32, 4),
    ] {
        assert_eq!(DType::try_from(name), Ok(dtype));
        let array = array(dtype, [1, 2, 3]);
        assert_eq!(array.dtype(), dtype);
        assert_eq!(array.shape(), [1, 2, 3]);
        assert_eq!(array.byte_strides(), [6 * width, 3 * width, width]);
        assert_eq!(array.offset(), 0);
        assert_eq!(array.byte_length(), 6 * width);
        assert_eq!(array.layout(), ArrayLayout::HwcInterleaved);
        assert_eq!(array.byte_order(), ByteOrder::LittleEndian);
        assert_eq!(array.memory_domain(), MemoryDomain::Host);
        assert!(array.read_only());
        assert_eq!(array.bytes().len(), array.byte_length());
        assert!(Arc::ptr_eq(
            &array.shared_bytes(),
            &array.clone().shared_bytes()
        ));
    }
}

#[test]
fn ndarray_rejects_unsupported_dtype_names() {
    for name in ["f64", "i32", "", "U16"] {
        assert_eq!(
            DType::try_from(name),
            Err(TransportError::UnsupportedDType(name.into()))
        );
    }
}

#[test]
fn ndarray_accepts_planar_prefix_and_padded_rows() {
    let mut planar = descriptor(DType::U16, [2, 2, 3]);
    planar.layout = ArrayLayout::Planar;
    planar.byte_strides = vec![4, 2, 8];
    let array = NdArray::new(vec![0; 24].into(), planar).unwrap();
    assert_eq!(array.layout(), ArrayLayout::Planar);
    assert_eq!(array.shape(), [2, 2, 3]);

    let mut padded = descriptor(DType::U16, [2, 2, 3]);
    padded.offset = 2;
    padded.byte_strides[0] = 16;
    padded.byte_length = 30; // two prefix bytes + row gap + final addressed row
    padded.read_only = false;
    let array = NdArray::new(vec![0; 30].into(), padded).unwrap();
    assert_eq!(array.offset(), 2);
    assert_eq!(array.byte_length(), 30);
    assert!(!array.read_only());
}

#[test]
fn ndarray_rejects_shape_and_stride_multiplication_overflow() {
    let mut shape = descriptor(DType::U16, [1, 2, 3]);
    shape.shape[0] = usize::MAX;
    assert_eq!(
        NdArray::new(vec![0; 12].into(), shape),
        Err(TransportError::SizeOverflow)
    );
    let mut stride = descriptor(DType::U16, [2, 1, 3]);
    stride.byte_strides[0] = usize::MAX;
    assert_eq!(
        NdArray::new(vec![0; 12].into(), stride),
        Err(TransportError::SizeOverflow)
    );
}

#[test]
fn ndarray_rejects_out_of_bounds_offset() {
    for offset in [12, 13, usize::MAX] {
        let mut d = descriptor(DType::U16, [1, 2, 3]);
        d.offset = offset;
        assert_eq!(
            NdArray::new(vec![0; 12].into(), d),
            Err(TransportError::OffsetOutOfBounds {
                offset,
                byte_length: 12
            })
        );
    }
}

#[test]
fn ndarray_rejects_declared_and_geometric_byte_length_mismatches() {
    let d = descriptor(DType::U16, [1, 2, 3]);
    assert_eq!(
        NdArray::new(vec![0; 11].into(), d),
        Err(TransportError::ByteLengthMismatch {
            expected: 12,
            actual: 11
        })
    );
    for byte_length in [11, 13] {
        let mut d = descriptor(DType::U16, [1, 2, 3]);
        d.byte_length = byte_length;
        assert_eq!(
            NdArray::new(vec![0; byte_length].into(), d),
            Err(TransportError::ByteLengthMismatch {
                expected: 12,
                actual: byte_length
            })
        );
    }
    let mut d = descriptor(DType::U16, [1, 2, 3]);
    d.offset = 2;
    assert_eq!(
        NdArray::new(vec![0; 12].into(), d),
        Err(TransportError::ByteLengthMismatch {
            expected: 14,
            actual: 12
        })
    );
}

#[test]
fn ndarray_rejects_empty_dimensions_invalid_rank_and_stride_rank() {
    for shape in [vec![], vec![0, 2, 3], vec![1; 9]] {
        let mut d = descriptor(DType::U16, [1, 2, 3]);
        d.shape = shape;
        assert_eq!(
            NdArray::new(vec![0; 12].into(), d),
            Err(TransportError::InvalidShape)
        );
    }
    let mut d = descriptor(DType::U16, [1, 2, 3]);
    d.byte_strides.pop();
    assert_eq!(
        NdArray::new(vec![0; 12].into(), d),
        Err(TransportError::StrideRankMismatch)
    );
}

#[test]
fn ndarray_rejects_overlapping_unaligned_and_incorrect_layout_strides() {
    for strides in [vec![12, 2, 2], vec![12, 6, 1], vec![0, 6, 2]] {
        let mut d = descriptor(DType::U16, [1, 2, 3]);
        d.byte_strides = strides;
        assert_eq!(
            NdArray::new(vec![0; 12].into(), d),
            Err(TransportError::InvalidStrides)
        );
    }
    let mut d = descriptor(DType::U16, [2, 2, 3]);
    d.layout = ArrayLayout::Planar;
    assert_eq!(
        NdArray::new(vec![0; 24].into(), d),
        Err(TransportError::InvalidStrides)
    );
}

#[test]
fn metadata_supports_the_four_namespaces_and_nested_values() {
    let doc = metadata(&[
        ("source.frame_id", Json::Str("0001".into())),
        (
            "color.profile",
            Json::obj(vec![("matrix", Json::Array(vec![Json::Float(1.0)]))]),
        ),
        ("exposure.ev", Json::Float(-0.5)),
        ("audit.verified", Json::Bool(true)),
    ]);
    assert_eq!(doc.fields().len(), 4);
    assert_eq!(doc.get("exposure.ev"), Some(&Json::Float(-0.5)));
}

#[test]
fn metadata_rejects_unqualified_unknown_and_empty_namespace_names() {
    for key in [
        "width",
        "dng.width",
        "source.",
        "color..space",
        "audit.bad key",
    ] {
        assert_eq!(
            MetadataDoc::new(vec![(key.into(), Json::Int(1))]),
            Err(TransportError::InvalidMetadataKey(key.into()))
        );
    }
}

#[test]
fn metadata_rejects_nonfinite_and_ambiguous_nested_values() {
    for value in [
        Json::Float(f64::NAN),
        Json::Array(vec![Json::Float(f64::INFINITY)]),
        Json::obj(vec![("a", Json::Int(1)), ("a", Json::Int(2))]),
    ] {
        assert_eq!(
            MetadataDoc::new(vec![("audit.value".into(), value)]),
            Err(TransportError::InvalidMetadataValue("audit.value".into()))
        );
    }
}

#[test]
fn metadata_rejects_conflicting_duplicates_but_allows_identical_evidence() {
    assert_eq!(
        MetadataDoc::new(vec![
            ("source.width".into(), Json::Int(2)),
            ("source.width".into(), Json::Int(3))
        ]),
        Err(TransportError::MetadataConflict("source.width".into()))
    );
    let same = MetadataDoc::new(vec![
        ("source.width".into(), Json::Int(2)),
        ("source.width".into(), Json::Int(2)),
    ])
    .unwrap();
    assert_eq!(same.fields().len(), 1);
}

#[test]
fn every_semantic_type_accepts_a_valid_envelope_and_rejects_transport_mismatch() {
    assert_eq!(SemanticRegistry::builtins().len(), 7);
    for schema in SemanticRegistry::builtins() {
        let envelope = DataEnvelope::new(
            schema.id,
            payload(schema.id),
            semantic_metadata(schema.id),
            provenance(),
        )
        .unwrap();
        assert_eq!(envelope.semantic_type(), schema.id);
        assert_eq!(
            envelope.transport_kind(),
            payload(schema.id).transport_kind()
        );
        let wrong = if schema.array.is_some() {
            Payload::DiskArtifact(
                DiskArtifact::new("pixels.bin", "application/octet-stream", None, None).unwrap(),
            )
        } else {
            Payload::NdArray(array(DType::U8, [1, 2, 3]))
        };
        let actual = wrong.transport_kind();
        assert_eq!(
            DataEnvelope::new(schema.id, wrong, semantic_metadata(schema.id), provenance()),
            Err(TransportError::TransportMismatch {
                semantic_type: schema.id.into(),
                actual
            })
        );
    }
}

#[test]
fn sidecar_semantics_allow_disk_and_memory_without_implicit_load() {
    let scratch = Scratch::new();
    for id in [FRAME_METADATA, ANALYSIS_SIDECAR] {
        let path = scratch.path(id);
        let reference = DiskArtifact::new(&path, "application/json", None, None).unwrap();
        let envelope = DataEnvelope::new(
            id,
            Payload::DiskArtifact(reference),
            semantic_metadata(id),
            provenance(),
        )
        .unwrap();
        assert_eq!(envelope.transport_kind(), TransportKind::DiskArtifact);
        assert!(!path.exists());
        assert!(matches!(envelope.payload(), Payload::DiskArtifact(_)));
        let memory =
            DataEnvelope::new(id, payload(id), semantic_metadata(id), provenance()).unwrap();
        assert_eq!(memory.transport_kind(), TransportKind::MemoryFile);
    }
}

#[test]
fn every_semantic_type_rejects_each_missing_required_field() {
    for schema in SemanticRegistry::builtins() {
        let valid = semantic_metadata(schema.id);
        for missing in schema.required_metadata {
            let doc = MetadataDoc::new(
                valid
                    .fields()
                    .iter()
                    .filter(|(key, _)| key.as_str() != missing.name)
                    .map(|(k, v)| (k.clone(), v.clone())),
            )
            .unwrap();
            assert_eq!(
                DataEnvelope::new(schema.id, payload(schema.id), doc, provenance()),
                Err(TransportError::MissingMetadata(missing.name.into()))
            );
        }
    }
}

#[test]
fn required_metadata_rejects_null_empty_text_and_wrong_numeric_types() {
    for (key, value) in [
        ("source.width", Json::Null),
        ("source.width", Json::Int(0)),
        ("source.height", Json::Int(-1)),
        ("source.height", Json::Float(1.0)),
        ("color.space", Json::Str(" ".into())),
        ("color.transfer", Json::Bool(true)),
    ] {
        let mut fields = frame_metadata().fields().clone();
        fields.insert(key.into(), value);
        let doc = MetadataDoc::new(fields).unwrap();
        assert_eq!(
            DataEnvelope::new(RGB48_FRAME, payload(RGB48_FRAME), doc, provenance()),
            Err(TransportError::InvalidMetadataValue(key.into()))
        );
    }
}

#[test]
fn rgb_semantics_reject_wrong_supported_dtypes() {
    for (id, expected) in [(RGB48_FRAME, DType::U16), (RGB24_FRAME, DType::U8)] {
        for actual in [DType::U8, DType::U16, DType::I16, DType::F32] {
            if actual == expected {
                continue;
            }
            assert_eq!(
                DataEnvelope::new(
                    id,
                    Payload::NdArray(array(actual, [1, 2, 3])),
                    frame_metadata(),
                    provenance()
                ),
                Err(TransportError::DTypeMismatch { expected, actual })
            );
        }
    }
}

#[test]
fn rgb_semantics_reject_geometry_mismatch_even_with_identical_byte_count() {
    for (id, dtype) in [(RGB48_FRAME, DType::U16), (RGB24_FRAME, DType::U8)] {
        for shape in [[2, 1, 3], [1, 3, 2]] {
            assert_eq!(
                DataEnvelope::new(
                    id,
                    Payload::NdArray(array(dtype, shape)),
                    frame_metadata(),
                    provenance()
                ),
                Err(TransportError::ShapeMismatch {
                    expected: vec![1, 2, 3],
                    actual: shape.to_vec()
                })
            );
        }
    }
}

#[test]
fn rgb_semantics_reject_planar_layout_and_wrong_rgb48_byte_order() {
    for (id, dtype) in [(RGB48_FRAME, DType::U16), (RGB24_FRAME, DType::U8)] {
        let mut d = descriptor(dtype, [1, 2, 3]);
        d.layout = ArrayLayout::Planar;
        let width = dtype.byte_width();
        d.byte_strides = vec![2 * width, width, 2 * width];
        let array = NdArray::new(vec![0; d.byte_length].into(), d).unwrap();
        assert_eq!(
            DataEnvelope::new(id, Payload::NdArray(array), frame_metadata(), provenance()),
            Err(TransportError::LayoutMismatch)
        );
    }
    let mut d = descriptor(DType::U16, [1, 2, 3]);
    d.byte_order = ByteOrder::BigEndian;
    let array = NdArray::new(vec![0; d.byte_length].into(), d).unwrap();
    assert_eq!(
        DataEnvelope::new(
            RGB48_FRAME,
            Payload::NdArray(array),
            frame_metadata(),
            provenance()
        ),
        Err(TransportError::ByteOrderMismatch)
    );
}

#[test]
fn unknown_semantic_type_is_rejected() {
    assert_eq!(
        SemanticRegistry::get("rawtl.unknown@1"),
        Err(TransportError::UnknownSemanticType(
            "rawtl.unknown@1".into()
        ))
    );
}

#[test]
fn envelope_patch_creates_a_new_identity_and_preserves_payload_and_lineage() {
    let original_provenance = Provenance::new(
        "rawtl.frame-reader",
        vec!["source-hash".into()],
        vec!["earlier-value".into()],
    )
    .unwrap();
    let original = DataEnvelope::new(
        RGB48_FRAME,
        payload(RGB48_FRAME),
        frame_metadata(),
        original_provenance,
    )
    .unwrap();
    let patch = MetadataPatch::new(vec![
        ("audit.roundtrip".into(), Json::Bool(true)),
        ("source.width".into(), Json::Int(2)),
    ])
    .unwrap();
    let patched = original.patch(&patch, "rawtl.roundtrip-check").unwrap();
    assert_ne!(patched.value_id(), original.value_id());
    assert_eq!(patched.semantic_type(), original.semantic_type());
    assert_eq!(patched.transport_kind(), original.transport_kind());
    assert_eq!(patched.payload(), original.payload());
    let (Payload::NdArray(before), Payload::NdArray(after)) =
        (original.payload(), patched.payload())
    else {
        panic!("expected arrays")
    };
    assert!(Arc::ptr_eq(&before.shared_bytes(), &after.shared_bytes()));
    assert_eq!(patched.provenance().producer(), "rawtl.roundtrip-check");
    assert_eq!(
        patched.provenance().source_hashes(),
        original.provenance().source_hashes()
    );
    assert_eq!(
        patched.provenance().parent_value_ids(),
        &["earlier-value", original.value_id()]
    );
    assert_eq!(
        patched.metadata().get("audit.roundtrip"),
        Some(&Json::Bool(true))
    );
    assert_eq!(original.metadata().get("audit.roundtrip"), None);
    assert_eq!(original.provenance().producer(), "rawtl.frame-reader");
    assert_eq!(original.clone().value_id(), original.value_id());
}

#[test]
fn envelope_patch_rejects_conflicts_atomically_in_all_namespaces() {
    let doc = frame_metadata()
        .patched(
            &MetadataPatch::new(vec![
                ("exposure.ev".into(), Json::Float(0.0)),
                ("audit.checked".into(), Json::Bool(true)),
            ])
            .unwrap(),
        )
        .unwrap();
    let original = DataEnvelope::new(RGB48_FRAME, payload(RGB48_FRAME), doc, provenance()).unwrap();
    let snapshot = original.clone();
    for (key, value) in [
        ("source.width", Json::Int(3)),
        ("color.space", Json::Str("BT.709".into())),
        ("exposure.ev", Json::Float(1.0)),
        ("audit.checked", Json::Bool(false)),
    ] {
        let patch = MetadataPatch::new(vec![
            ("audit.added".into(), Json::Bool(true)),
            (key.into(), value),
        ])
        .unwrap();
        assert_eq!(
            original.patch(&patch, "rawtl.check"),
            Err(TransportError::MetadataConflict(key.into()))
        );
        assert_eq!(original, snapshot);
    }
}

#[test]
fn empty_patch_still_creates_new_envelope_and_records_direct_parent() {
    let original = DataEnvelope::new(
        RGB24_FRAME,
        payload(RGB24_FRAME),
        frame_metadata(),
        provenance(),
    )
    .unwrap();
    let patched = original
        .patch(&MetadataPatch::new(vec![]).unwrap(), "rawtl.forward")
        .unwrap();
    assert_ne!(patched.value_id(), original.value_id());
    assert_eq!(patched.metadata(), original.metadata());
    assert_eq!(
        patched.provenance().parent_value_ids(),
        &[original.value_id()]
    );
}

#[test]
fn provenance_requires_a_producer_and_source_hash_binding() {
    assert_eq!(
        Provenance::new(" ", vec![], vec![]),
        Err(TransportError::EmptyProducer)
    );
    let wrong =
        Provenance::new("rawtl.analysis", vec!["wrong-source-hash".into()], vec![]).unwrap();
    assert_eq!(
        DataEnvelope::new(
            ANALYSIS_SIDECAR,
            payload(ANALYSIS_SIDECAR),
            semantic_metadata(ANALYSIS_SIDECAR),
            wrong
        ),
        Err(TransportError::SourceHashMismatch("source-hash".into()))
    );
    let original = DataEnvelope::new(
        RGB24_FRAME,
        payload(RGB24_FRAME),
        frame_metadata(),
        provenance(),
    )
    .unwrap();
    assert_eq!(
        original.patch(&MetadataPatch::new(vec![]).unwrap(), ""),
        Err(TransportError::EmptyProducer)
    );
}

#[test]
fn independent_envelopes_have_distinct_generated_ids() {
    let mut ids = std::collections::BTreeSet::new();
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    (0..64)
                        .map(|_| {
                            DataEnvelope::new(
                                RGB24_FRAME,
                                payload(RGB24_FRAME),
                                frame_metadata(),
                                provenance(),
                            )
                            .unwrap()
                            .value_id()
                            .to_owned()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        for handle in handles {
            for id in handle.join().unwrap() {
                assert!(ids.insert(id));
            }
        }
    });
    assert_eq!(ids.len(), 512);
}

#[test]
fn contract_errors_have_actionable_messages() {
    assert_eq!(
        TransportError::MetadataConflict("source.width".into()).to_string(),
        "conflicting authoritative metadata: source.width"
    );
    assert_eq!(
        TransportError::SizeOverflow.to_string(),
        "array shape/strides/offset size overflow"
    );
    assert_eq!(
        TransportError::ByteLengthMismatch {
            expected: 12,
            actual: 11
        }
        .to_string(),
        "array byte length 11 does not match 12"
    );
    assert_eq!(
        TransportError::EmptyPath.to_string(),
        "artifact path must not be empty"
    );
}
