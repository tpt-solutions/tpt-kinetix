use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tpt_kinetix_av1::entropy::{BlockMarker, SymbolTraceEntry};
use tpt_kinetix_h264::TracePlane;

use crate::trace_dump::{MapTracer, MbInfo, Stage};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceKey {
    pub mb_x: u32,
    pub mb_y: u32,
    pub plane: String,
    pub blk: u8,
    pub stage: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TraceValue {
    Integers(Vec<i32>),
    Text(String),
    /// The matching key was absent from one side of a comparison.
    Missing,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TraceCapture {
    pub entries: BTreeMap<String, TraceValue>,
}

fn plane_name(plane: TracePlane) -> &'static str {
    match plane {
        TracePlane::Luma => "luma",
        TracePlane::Cb => "cb",
        TracePlane::Cr => "cr",
    }
}

fn stage_name(stage: Stage) -> &'static str {
    match stage {
        Stage::CavlcCoeffs => "cavlc_coeffs",
        Stage::CavlcBlockInfo => "cavlc_block_info",
        Stage::IntraPred => "intra_pred",
        Stage::Reconstructed => "reconstructed",
        Stage::Deblocked => "deblocked",
    }
}

fn key_string(key: &TraceKey) -> String {
    format!(
        "mb({},{}):{}:{}:{}",
        key.mb_x, key.mb_y, key.plane, key.blk, key.stage
    )
}

impl From<&MapTracer> for TraceCapture {
    fn from(tracer: &MapTracer) -> Self {
        let mut entries = BTreeMap::new();
        for (key, values) in &tracer.values {
            let key = TraceKey {
                mb_x: key.mb_x,
                mb_y: key.mb_y,
                plane: plane_name(key.plane).to_string(),
                blk: key.blk,
                stage: stage_name(key.stage).to_string(),
            };
            entries.insert(key_string(&key), TraceValue::Integers(values.clone()));
        }
        for (key, info) in &tracer.block_info {
            let key = TraceKey {
                mb_x: key.mb_x,
                mb_y: key.mb_y,
                plane: plane_name(key.plane).to_string(),
                blk: key.blk,
                stage: stage_name(key.stage).to_string(),
            };
            entries.insert(
                key_string(&key),
                TraceValue::Text(format!(
                    "n_c={},total_coeff={},trailing_ones={},suffix_len={}",
                    info.n_c, info.total_coeff, info.trailing_ones, info.suffix_len
                )),
            );
        }
        for ((mb_x, mb_y), info) in &tracer.mb_info {
            entries.insert(
                format!("mb({},{}):metadata", mb_x, mb_y),
                TraceValue::Text(mb_info_text(info)),
            );
        }
        Self { entries }
    }
}

fn mb_info_text(info: &MbInfo) -> String {
    format!(
        "type={},qp={},cbp={},chroma={},modes={:?}",
        info.mb_type, info.qp, info.cbp, info.intra_chroma_pred_mode, info.pred_modes
    )
}

impl TraceCapture {
    pub fn from_av1_trace(trace: &[SymbolTraceEntry], markers: &[BlockMarker]) -> Self {
        let mut entries = BTreeMap::new();
        for entry in trace {
            entries.insert(
                format!("av1:symbol:{}", entry.seq),
                TraceValue::Text(format!(
                    "n_symbols={},value={},bits=[{},{}),range={},value_state={},location={}",
                    entry.n_symbols,
                    entry.value,
                    entry.bit_pos_before,
                    entry.bit_pos_after,
                    entry.sym_range,
                    entry.sym_value,
                    entry.location
                )),
            );
        }
        for marker in markers {
            entries.insert(
                format!("av1:marker:{}", marker.trace_seq),
                TraceValue::Text(marker.label.clone()),
            );
        }
        Self { entries }
    }
}

pub fn to_json(capture: &TraceCapture) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(capture)
}

pub fn from_json(json: &str) -> Result<TraceCapture, serde_json::Error> {
    serde_json::from_str(json)
}

/// Return the first lexicographically ordered key whose values differ.
///
/// A key present on only one side is itself a divergence and is returned with
/// [`TraceValue::Missing`] for the absent side. This prevents a partial trace
/// from being reported as identical to a complete trace.
pub fn first_divergence(
    left: &TraceCapture,
    right: &TraceCapture,
) -> Option<(String, TraceValue, TraceValue)> {
    let mut keys = std::collections::BTreeSet::new();
    keys.extend(left.entries.keys().cloned());
    keys.extend(right.entries.keys().cloned());
    keys.into_iter().find_map(|key| {
        let left_value = left.entries.get(&key).cloned();
        let right_value = right.entries.get(&key).cloned();
        (left_value != right_value).then(|| {
            (
                key,
                left_value.unwrap_or(TraceValue::Missing),
                right_value.unwrap_or(TraceValue::Missing),
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tpt_kinetix_h264::DecodeTracer;

    #[test]
    fn map_tracer_round_trips_json() {
        let mut tracer = MapTracer::new();
        tracer.on_reconstructed(0, 0, TracePlane::Luma, 0, &[1, 2, 3, 4]);
        let capture = TraceCapture::from(&tracer);
        let json = to_json(&capture).unwrap();
        assert_eq!(from_json(&json).unwrap(), capture);
    }

    #[test]
    fn av1_trace_converts_to_shared_capture() {
        let trace = [SymbolTraceEntry {
            seq: 7,
            n_symbols: 4,
            value: 2,
            bit_pos_before: 10,
            bit_pos_after: 13,
            sym_range: 123,
            sym_value: 456,
            location: std::panic::Location::caller(),
        }];
        let markers = [BlockMarker {
            trace_seq: 7,
            label: "mode_info mi=(0,0)".to_string(),
        }];
        let capture = TraceCapture::from_av1_trace(&trace, &markers);
        assert!(capture.entries.contains_key("av1:symbol:7"));
        assert!(capture.entries.contains_key("av1:marker:7"));
    }

    #[test]
    fn finds_first_value_divergence() {
        let left = TraceCapture {
            entries: BTreeMap::from([("a".into(), TraceValue::Integers(vec![1]))]),
        };
        let right = TraceCapture {
            entries: BTreeMap::from([("a".into(), TraceValue::Integers(vec![2]))]),
        };
        assert_eq!(first_divergence(&left, &right).unwrap().0, "a");
    }

    #[test]
    fn missing_key_is_a_divergence() {
        let left = TraceCapture {
            entries: BTreeMap::from([("a".into(), TraceValue::Integers(vec![1]))]),
        };
        let right = TraceCapture::default();
        assert_eq!(
            first_divergence(&left, &right),
            Some((
                "a".into(),
                TraceValue::Integers(vec![1]),
                TraceValue::Missing,
            ))
        );
    }
}
