//! Immutable in-process carriers and product semantics. Only `persist` and
//! `load` perform I/O; wrapping or validating a carrier never converts it.
//!
//! Encoded bytes cannot implicitly become a disk reference:
//! ```compile_fail,E0277
//! use rawtl_linear_video::transport::{DiskArtifact, MemoryFile};
//! let file = MemoryFile::new(vec![1, 2, 3].into(), "image/png").unwrap();
//! let artifact: DiskArtifact = file.into();
//! ```
//! A disk reference cannot implicitly read a file into memory either:
//! ```compile_fail,E0277
//! use rawtl_linear_video::transport::{DiskArtifact, MemoryFile};
//! let artifact = DiskArtifact::new("frame.png", "image/png", None, None).unwrap();
//! let file: MemoryFile = artifact.into();
//! ```

use crate::json::Json;
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, TransportError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    EmptyPath,
    EmptyMediaType,
    UnsupportedDType(String),
    InvalidShape,
    StrideRankMismatch,
    InvalidStrides,
    SizeOverflow,
    OffsetOutOfBounds {
        offset: usize,
        byte_length: usize,
    },
    ByteLengthMismatch {
        expected: usize,
        actual: usize,
    },
    InvalidMetadataKey(String),
    InvalidMetadataValue(String),
    MetadataConflict(String),
    UnknownSemanticType(String),
    TransportMismatch {
        semantic_type: String,
        actual: TransportKind,
    },
    DTypeMismatch {
        expected: DType,
        actual: DType,
    },
    ShapeMismatch {
        expected: Vec<usize>,
        actual: Vec<usize>,
    },
    LayoutMismatch,
    ByteOrderMismatch,
    MissingMetadata(String),
    EmptyProducer,
    SourceHashMismatch(String),
    ArtifactSizeMismatch {
        expected: u64,
        actual: u64,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        kind: io::ErrorKind,
        message: String,
    },
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath => write!(f, "artifact path must not be empty"),
            Self::EmptyMediaType => write!(f, "media/content type must not be empty"),
            Self::UnsupportedDType(dtype) => write!(f, "unsupported dtype: {dtype}"),
            Self::InvalidShape => write!(f, "shape must have 1 to 8 nonzero dimensions"),
            Self::StrideRankMismatch => write!(f, "shape and byte strides must have equal rank"),
            Self::InvalidStrides => write!(
                f,
                "byte strides must be aligned, nonoverlapping and match layout"
            ),
            Self::SizeOverflow => write!(f, "array shape/strides/offset size overflow"),
            Self::OffsetOutOfBounds {
                offset,
                byte_length,
            } => write!(f, "array offset {offset} is outside {byte_length} bytes"),
            Self::ByteLengthMismatch { expected, actual } => {
                write!(f, "array byte length {actual} does not match {expected}")
            }
            Self::InvalidMetadataKey(key) => write!(
                f,
                "metadata key requires source/color/exposure/audit namespace: {key}"
            ),
            Self::InvalidMetadataValue(key) => write!(f, "invalid metadata value: {key}"),
            Self::MetadataConflict(key) => write!(f, "conflicting authoritative metadata: {key}"),
            Self::UnknownSemanticType(id) => write!(f, "unknown semantic type: {id}"),
            Self::TransportMismatch {
                semantic_type,
                actual,
            } => write!(f, "transport {actual:?} is not allowed for {semantic_type}"),
            Self::DTypeMismatch { expected, actual } => {
                write!(f, "dtype {actual:?} does not match {expected:?}")
            }
            Self::ShapeMismatch { expected, actual } => write!(
                f,
                "shape {actual:?} does not match metadata shape {expected:?}"
            ),
            Self::LayoutMismatch => write!(f, "RGB frame requires HWC interleaved layout"),
            Self::ByteOrderMismatch => write!(f, "RGB48 frame requires little-endian samples"),
            Self::MissingMetadata(key) => write!(f, "missing required metadata: {key}"),
            Self::EmptyProducer => write!(f, "provenance producer must not be empty"),
            Self::SourceHashMismatch(hash) => {
                write!(f, "source hash absent from provenance: {hash}")
            }
            Self::ArtifactSizeMismatch { expected, actual } => write!(
                f,
                "artifact size {actual} does not match declared {expected}"
            ),
            Self::Io {
                operation,
                path,
                message,
                ..
            } => write!(f, "{operation} {}: {message}", path.display()),
        }
    }
}

impl std::error::Error for TransportError {}

fn io_error(operation: &'static str, path: &Path, error: io::Error) -> TransportError {
    TransportError::Io {
        operation,
        path: path.to_owned(),
        kind: error.kind(),
        message: error.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    DiskArtifact,
    MemoryFile,
    NdArray,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskArtifact {
    path: PathBuf,
    media_type: String,
    size: Option<u64>,
    hash: Option<String>,
}

impl DiskArtifact {
    /// Construct a reference, without stat/read/write. Existence is checked by
    /// the explicit reader or the producing stage, not inferred from its name.
    /// Size and hash are optional declarations, not freshly verified evidence.
    pub fn new(
        path: impl Into<PathBuf>,
        media_type: impl Into<String>,
        size: Option<u64>,
        hash: Option<String>,
    ) -> Result<Self> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(TransportError::EmptyPath);
        }
        let media_type = checked_media_type(media_type.into())?;
        Ok(Self {
            path,
            media_type,
            size,
            hash,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn media_type(&self) -> &str {
        &self.media_type
    }
    pub fn size(&self) -> Option<u64> {
        self.size
    }
    pub fn hash(&self) -> Option<&str> {
        self.hash.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryFile {
    bytes: Arc<[u8]>,
    content_type: String,
}

impl MemoryFile {
    pub fn new(bytes: Arc<[u8]>, content_type: impl Into<String>) -> Result<Self> {
        Ok(Self {
            bytes,
            content_type: checked_media_type(content_type.into())?,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn shared_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }
    pub fn content_type(&self) -> &str {
        &self.content_type
    }
    pub fn len(&self) -> usize {
        self.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

fn checked_media_type(value: String) -> Result<String> {
    if value.trim().is_empty() {
        Err(TransportError::EmptyMediaType)
    } else {
        Ok(value)
    }
}

/// Explicitly materialize encoded bytes. Existing paths are never overwritten.
/// A failed write can leave a partial new file; no artifact is returned for it.
pub fn persist(file: &MemoryFile, path: impl Into<PathBuf>) -> Result<DiskArtifact> {
    let artifact = DiskArtifact::new(path, file.content_type(), Some(file.len() as u64), None)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(artifact.path())
        .map_err(|e| io_error("persist", artifact.path(), e))?;
    output
        .write_all(file.bytes())
        .map_err(|e| io_error("persist", artifact.path(), e))?;
    Ok(artifact)
}

/// Explicitly read encoded bytes, retaining media type and checking a declared
/// size when present. Hash verification remains the caller's existing policy;
/// an optional hash on a reference is not a claim made by this reader.
pub fn load(artifact: &DiskArtifact) -> Result<MemoryFile> {
    let bytes = fs::read(artifact.path()).map_err(|e| io_error("load", artifact.path(), e))?;
    if let Some(expected) = artifact.size() {
        if expected != bytes.len() as u64 {
            return Err(TransportError::ArtifactSizeMismatch {
                expected,
                actual: bytes.len() as u64,
            });
        }
    }
    MemoryFile::new(bytes.into(), artifact.media_type())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    U8,
    U16,
    I16,
    F32,
}

impl DType {
    pub fn byte_width(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::U16 | Self::I16 => 2,
            Self::F32 => 4,
        }
    }
}

impl TryFrom<&str> for DType {
    type Error = TransportError;
    fn try_from(value: &str) -> Result<Self> {
        match value {
            "u8" => Ok(Self::U8),
            "u16" => Ok(Self::U16),
            "i16" => Ok(Self::I16),
            "f32" => Ok(Self::F32),
            _ => Err(TransportError::UnsupportedDType(value.into())),
        }
    }
}

/// Shape coordinates are always [height, width, channels] for rank-three
/// arrays. Planar changes storage order, not the meaning of shape coordinates.
/// Other ranks use conventional last-axis-first storage for either layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayLayout {
    HwcInterleaved,
    Planar,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteOrder {
    LittleEndian,
    BigEndian,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryDomain {
    Host,
}

/// Declared byte_length is the entire backing length. It must equal both the
/// actual allocation length and offset + addressed span (including padding).
/// Prefix bytes and padded strides are allowed; unused trailing bytes, empty
/// axes, negative/broadcast/overlapping strides are outside this contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrayDescriptor {
    pub dtype: DType,
    pub shape: Vec<usize>,
    pub byte_strides: Vec<usize>,
    pub offset: usize,
    pub byte_length: usize,
    pub layout: ArrayLayout,
    pub byte_order: ByteOrder,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdArray {
    bytes: Arc<[u8]>,
    descriptor: ArrayDescriptor,
}

impl NdArray {
    pub fn new(bytes: Arc<[u8]>, descriptor: ArrayDescriptor) -> Result<Self> {
        let d = &descriptor;
        if d.shape.is_empty() || d.shape.len() > 8 || d.shape.contains(&0) {
            return Err(TransportError::InvalidShape);
        }
        if d.byte_strides.len() != d.shape.len() {
            return Err(TransportError::StrideRankMismatch);
        }
        if d.byte_length != bytes.len() {
            return Err(TransportError::ByteLengthMismatch {
                expected: d.byte_length,
                actual: bytes.len(),
            });
        }
        if d.offset >= d.byte_length {
            return Err(TransportError::OffsetOutOfBounds {
                offset: d.offset,
                byte_length: d.byte_length,
            });
        }
        let width = d.dtype.byte_width();
        // Check both logical size and every dimension * stride, even when a
        // singleton axis would contribute no displacement to the final span.
        let mut logical_length = width;
        for (&dimension, &stride) in d.shape.iter().zip(&d.byte_strides) {
            logical_length = logical_length
                .checked_mul(dimension)
                .ok_or(TransportError::SizeOverflow)?;
            dimension
                .checked_mul(stride)
                .ok_or(TransportError::SizeOverflow)?;
        }
        let axes: Vec<usize> = if d.layout == ArrayLayout::Planar && d.shape.len() == 3 {
            vec![1, 0, 2]
        } else {
            (0..d.shape.len()).rev().collect()
        };
        let mut span = width;
        for axis in axes {
            let stride = d.byte_strides[axis];
            if stride < span || stride % width != 0 {
                return Err(TransportError::InvalidStrides);
            }
            span = (d.shape[axis] - 1)
                .checked_mul(stride)
                .and_then(|displacement| span.checked_add(displacement))
                .ok_or(TransportError::SizeOverflow)?;
        }
        let expected = d
            .offset
            .checked_add(span)
            .ok_or(TransportError::SizeOverflow)?;
        if expected != d.byte_length {
            return Err(TransportError::ByteLengthMismatch {
                expected,
                actual: d.byte_length,
            });
        }
        Ok(Self { bytes, descriptor })
    }
    pub fn descriptor(&self) -> &ArrayDescriptor {
        &self.descriptor
    }
    pub fn dtype(&self) -> DType {
        self.descriptor.dtype
    }
    pub fn shape(&self) -> &[usize] {
        &self.descriptor.shape
    }
    pub fn byte_strides(&self) -> &[usize] {
        &self.descriptor.byte_strides
    }
    pub fn offset(&self) -> usize {
        self.descriptor.offset
    }
    pub fn byte_length(&self) -> usize {
        self.descriptor.byte_length
    }
    pub fn layout(&self) -> ArrayLayout {
        self.descriptor.layout
    }
    pub fn byte_order(&self) -> ByteOrder {
        self.descriptor.byte_order
    }
    /// Descriptive intent only: even read_only=false never exposes shared
    /// mutable access. Producers allocate a new output to change samples.
    pub fn read_only(&self) -> bool {
        self.descriptor.read_only
    }
    pub fn memory_domain(&self) -> MemoryDomain {
        MemoryDomain::Host
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn shared_bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }
}

/// Every published field is authoritative: new facts may be added, existing
/// facts may only be repeated verbatim. There is no mutable map accessor.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MetadataDoc {
    fields: BTreeMap<String, Json>,
}

impl MetadataDoc {
    pub fn new(fields: impl IntoIterator<Item = (String, Json)>) -> Result<Self> {
        let mut doc = Self::default();
        for (key, value) in fields {
            let (namespace, name) = key
                .split_once('.')
                .ok_or_else(|| TransportError::InvalidMetadataKey(key.clone()))?;
            if !matches!(namespace, "source" | "color" | "exposure" | "audit")
                || name.is_empty()
                || name
                    .split('.')
                    .any(|part| part.is_empty() || part.chars().any(char::is_whitespace))
            {
                return Err(TransportError::InvalidMetadataKey(key));
            }
            if !valid_json(&value, 0) {
                return Err(TransportError::InvalidMetadataValue(key));
            }
            if let Some(previous) = doc.fields.get(&key) {
                if previous != &value {
                    return Err(TransportError::MetadataConflict(key));
                }
            }
            doc.fields.insert(key, value);
        }
        Ok(doc)
    }
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.fields.get(key)
    }
    pub fn fields(&self) -> &BTreeMap<String, Json> {
        &self.fields
    }
    pub fn patched(&self, patch: &MetadataPatch) -> Result<Self> {
        let mut result = self.clone();
        for (key, value) in patch.fields.fields() {
            if let Some(previous) = self.get(key) {
                if previous != value {
                    return Err(TransportError::MetadataConflict(key.clone()));
                }
            }
            result.fields.insert(key.clone(), value.clone());
        }
        Ok(result)
    }
}

fn valid_json(value: &Json, depth: usize) -> bool {
    if depth > 128 {
        return false;
    }
    match value {
        Json::Float(value) => value.is_finite(),
        Json::Array(values) => values.iter().all(|v| valid_json(v, depth + 1)),
        Json::Object(entries) => {
            let mut keys = std::collections::BTreeSet::new();
            entries
                .iter()
                .all(|(key, v)| keys.insert(key) && valid_json(v, depth + 1))
        }
        _ => true,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetadataPatch {
    fields: MetadataDoc,
}
impl MetadataPatch {
    pub fn new(fields: impl IntoIterator<Item = (String, Json)>) -> Result<Self> {
        Ok(Self {
            fields: MetadataDoc::new(fields)?,
        })
    }
    pub fn fields(&self) -> &MetadataDoc {
        &self.fields
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    producer: String,
    source_hashes: Vec<String>,
    parent_value_ids: Vec<String>,
}
impl Provenance {
    pub fn new(
        producer: impl Into<String>,
        source_hashes: Vec<String>,
        parent_value_ids: Vec<String>,
    ) -> Result<Self> {
        let producer = producer.into();
        if producer.trim().is_empty() {
            return Err(TransportError::EmptyProducer);
        }
        Ok(Self {
            producer,
            source_hashes,
            parent_value_ids,
        })
    }
    pub fn producer(&self) -> &str {
        &self.producer
    }
    pub fn source_hashes(&self) -> &[String] {
        &self.source_hashes
    }
    pub fn parent_value_ids(&self) -> &[String] {
        &self.parent_value_ids
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    DiskArtifact(DiskArtifact),
    MemoryFile(MemoryFile),
    NdArray(NdArray),
}
impl Payload {
    pub fn transport_kind(&self) -> TransportKind {
        match self {
            Self::DiskArtifact(_) => TransportKind::DiskArtifact,
            Self::MemoryFile(_) => TransportKind::MemoryFile,
            Self::NdArray(_) => TransportKind::NdArray,
        }
    }
}

pub const FRAME_PNG: &str = "rawtl.frame-png@1";
pub const FRAME_METADATA: &str = "rawtl.frame-metadata@1";
pub const ANALYSIS_SIDECAR: &str = "rawtl.analysis-sidecar@1";
pub const RGB48_FRAME: &str = "rawtl.rgb48-frame@1";
pub const RGB24_FRAME: &str = "rawtl.rgb24-frame@1";
pub const VIDEO_ARTIFACT: &str = "rawtl.video-artifact@1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataFieldType {
    NonemptyText,
    PositiveInteger,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequiredMetadataField {
    pub name: &'static str,
    pub field_type: MetadataFieldType,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayConstraints {
    pub dtype: DType,
    pub channels: usize,
    pub layout: ArrayLayout,
    pub byte_order: Option<ByteOrder>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticType {
    pub id: &'static str,
    pub allowed_transports: &'static [TransportKind],
    pub array: Option<ArrayConstraints>,
    pub required_metadata: &'static [RequiredMetadataField],
}

const fn integer(name: &'static str) -> RequiredMetadataField {
    RequiredMetadataField {
        name,
        field_type: MetadataFieldType::PositiveInteger,
    }
}
const fn text(name: &'static str) -> RequiredMetadataField {
    RequiredMetadataField {
        name,
        field_type: MetadataFieldType::NonemptyText,
    }
}
const FRAME_FIELDS: &[RequiredMetadataField] = &[
    integer("source.width"),
    integer("source.height"),
    text("color.space"),
    text("color.transfer"),
];
const SEMANTICS: &[SemanticType] = &[
    SemanticType {
        id: FRAME_PNG,
        allowed_transports: &[TransportKind::DiskArtifact],
        array: None,
        required_metadata: FRAME_FIELDS,
    },
    SemanticType {
        id: FRAME_METADATA,
        allowed_transports: &[TransportKind::DiskArtifact, TransportKind::MemoryFile],
        array: None,
        required_metadata: &[
            integer("source.schema_version"),
            integer("source.width"),
            integer("source.height"),
        ],
    },
    SemanticType {
        id: ANALYSIS_SIDECAR,
        allowed_transports: &[TransportKind::DiskArtifact, TransportKind::MemoryFile],
        array: None,
        required_metadata: &[
            integer("source.schema_version"),
            text("source.kind"),
            text("source.sha256"),
        ],
    },
    SemanticType {
        id: RGB48_FRAME,
        allowed_transports: &[TransportKind::NdArray],
        array: Some(ArrayConstraints {
            dtype: DType::U16,
            channels: 3,
            layout: ArrayLayout::HwcInterleaved,
            byte_order: Some(ByteOrder::LittleEndian),
        }),
        required_metadata: FRAME_FIELDS,
    },
    SemanticType {
        id: RGB24_FRAME,
        allowed_transports: &[TransportKind::NdArray],
        array: Some(ArrayConstraints {
            dtype: DType::U8,
            channels: 3,
            layout: ArrayLayout::HwcInterleaved,
            byte_order: None,
        }),
        required_metadata: FRAME_FIELDS,
    },
    SemanticType {
        id: VIDEO_ARTIFACT,
        allowed_transports: &[TransportKind::DiskArtifact],
        array: None,
        required_metadata: &[text("audit.codec"), integer("source.frame_count")],
    },
];

/// Fixed built-ins only; descriptors are queryable but cannot replace the
/// validation rules. JSON sidecar contents still need their existing parser.
pub struct SemanticRegistry;
impl SemanticRegistry {
    pub fn builtins() -> &'static [SemanticType] {
        SEMANTICS
    }
    pub fn get(id: &str) -> Result<&'static SemanticType> {
        SEMANTICS
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| TransportError::UnknownSemanticType(id.into()))
    }
    pub fn validate(id: &str, payload: &Payload, metadata: &MetadataDoc) -> Result<()> {
        let schema = Self::get(id)?;
        if !schema
            .allowed_transports
            .contains(&payload.transport_kind())
        {
            return Err(TransportError::TransportMismatch {
                semantic_type: id.into(),
                actual: payload.transport_kind(),
            });
        }
        for field in schema.required_metadata {
            let value = metadata
                .get(field.name)
                .ok_or_else(|| TransportError::MissingMetadata(field.name.into()))?;
            let valid = match field.field_type {
                MetadataFieldType::NonemptyText => {
                    matches!(value, Json::Str(v) if !v.trim().is_empty())
                }
                MetadataFieldType::PositiveInteger => matches!(value, Json::Int(v) if *v > 0),
            };
            if !valid {
                return Err(TransportError::InvalidMetadataValue(field.name.into()));
            }
        }
        if let (Some(constraints), Payload::NdArray(array)) = (schema.array, payload) {
            if array.dtype() != constraints.dtype {
                return Err(TransportError::DTypeMismatch {
                    expected: constraints.dtype,
                    actual: array.dtype(),
                });
            }
            let dimension = |key| match metadata.get(key) {
                Some(Json::Int(value)) => usize::try_from(*value)
                    .map_err(|_| TransportError::InvalidMetadataValue(key.into())),
                _ => Err(TransportError::InvalidMetadataValue(key.into())),
            };
            let expected = vec![
                dimension("source.height")?,
                dimension("source.width")?,
                constraints.channels,
            ];
            if array.shape() != expected {
                return Err(TransportError::ShapeMismatch {
                    expected,
                    actual: array.shape().to_vec(),
                });
            }
            if array.layout() != constraints.layout {
                return Err(TransportError::LayoutMismatch);
            }
            if constraints
                .byte_order
                .is_some_and(|order| order != array.byte_order())
            {
                return Err(TransportError::ByteOrderMismatch);
            }
        }
        Ok(())
    }
}

/// All fields (including the payload descriptor) are private. Cloning preserves
/// identity; patching creates a distinct identity and records the input parent.
/// ```compile_fail
/// use rawtl_linear_video::transport::DataEnvelope;
/// fn change_published_metadata(envelope: &mut DataEnvelope) {
///     envelope.metadata().fields().clear();
/// }
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct DataEnvelope {
    value_id: String,
    semantic_type: String,
    payload: Payload,
    metadata: MetadataDoc,
    provenance: Provenance,
}
impl DataEnvelope {
    pub fn new(
        semantic_type: impl Into<String>,
        payload: Payload,
        metadata: MetadataDoc,
        provenance: Provenance,
    ) -> Result<Self> {
        let semantic_type = semantic_type.into();
        SemanticRegistry::validate(&semantic_type, &payload, &metadata)?;
        if let Some(value) = metadata.get("source.sha256") {
            let hash = value
                .as_str()
                .filter(|v| !v.trim().is_empty())
                .ok_or_else(|| TransportError::InvalidMetadataValue("source.sha256".into()))?;
            if !provenance.source_hashes.iter().any(|source| source == hash) {
                return Err(TransportError::SourceHashMismatch(hash.into()));
            }
        }
        Ok(Self {
            value_id: generated_id(),
            semantic_type,
            payload,
            metadata,
            provenance,
        })
    }
    pub fn value_id(&self) -> &str {
        &self.value_id
    }
    pub fn transport_kind(&self) -> TransportKind {
        self.payload.transport_kind()
    }
    pub fn semantic_type(&self) -> &str {
        &self.semantic_type
    }
    pub fn payload(&self) -> &Payload {
        &self.payload
    }
    pub fn metadata(&self) -> &MetadataDoc {
        &self.metadata
    }
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
    }
    pub fn patch(&self, patch: &MetadataPatch, producer: impl Into<String>) -> Result<Self> {
        let metadata = self.metadata.patched(patch)?;
        let mut parents = self.provenance.parent_value_ids.clone();
        if !parents.contains(&self.value_id) {
            parents.push(self.value_id.clone());
        }
        let provenance = Provenance::new(producer, self.provenance.source_hashes.clone(), parents)?;
        Self::new(
            self.semantic_type.clone(),
            self.payload.clone(),
            metadata,
            provenance,
        )
    }
}

fn generated_id() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "rawtl-value-{}-{tick}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(test)]
mod tests;
