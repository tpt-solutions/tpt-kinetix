# tpt-kinetix-demux

Container demuxers exposing a uniform packet-extraction API via the `Demuxer`
trait.

- **MP4 / ISO-BMFF** (`mp4`) — full box parser, track/codec identification,
  sample-table timing, packet extraction.
- **Matroska / WebM** (`mkv`) — basic EBML reader: track enumeration and
  SimpleBlock/Block frame extraction (no seeking index or advanced lacing yet).

See the [workspace README](../README.md) for the full project overview,
architecture diagram, and quickstart guide.
