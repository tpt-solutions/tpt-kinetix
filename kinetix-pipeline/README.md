# kinetix-pipeline

Concurrent multi-stage processing pipeline connecting demux → decode → filter stages via
`crossbeam-channel` bounded channels with backpressure semantics.

See the [workspace README](../README.md) for the full project overview and quickstart guide.

## Architecture

```
Input bytes
    │
    ▼
┌──────────┐   Packet   ┌──────────────┐   VideoFrame  ┌──────────────┐   VideoFrame  ┌──────────┐
│  Demux   │──────────▶│    Decode    │──────────────▶│    Filter    │──────────────▶│   Sink   │
│  Stage   │ crossbeam  │    Stage     │  crossbeam    │    Stage     │  crossbeam    │  Stage   │
└──────────┘  channel   └──────────────┘   channel     └──────────────┘   channel     └──────────┘
```

Each stage runs in its own OS thread.  The bounded channels between stages act as
backpressure buffers (default capacity: 64 messages): a fast upstream stage blocks
when the buffer is full, preventing unbounded memory growth.

### Stage summary

| Stage | Input | Output | Implementation |
|-------|-------|--------|----------------|
| `DemuxStage` | (none — reads from `data: Vec<u8>`) | `Packet` | `kinetix_demux::Mp4Demuxer` |
| `DecodeStage` | `Packet` | `VideoFrame` | `kinetix_h264::H264Decoder` |
| `FilterStage` | `VideoFrame` | `VideoFrame` | Pluggable `Fn(VideoFrame) -> VideoFrame` |
| `SinkStage` | `VideoFrame` | (none — collects into `Arc<Mutex<Vec<VideoFrame>>>`) | Built-in collector |

### Flush propagation

A `PipelineMessage::Flush` flows from the source through every stage in order,
signalling each stage to drain its internal buffers and exit cleanly.  Once all
threads have joined, `Pipeline::run_to_completion` returns.

## Usage

```rust
use kinetix_pipeline::{Pipeline, DemuxStage, DecodeStage, FilterStage, SinkStage};

let (sink, frames) = SinkStage::new();

Pipeline::new()
    .add_stage(DemuxStage { data: mp4_bytes })
    .add_stage(DecodeStage)
    .add_stage(FilterStage::passthrough())
    .add_stage(sink)
    .run_to_completion()?;

let decoded = frames.lock().unwrap();
println!("decoded {} frames", decoded.len());
```
