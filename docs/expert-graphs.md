# Expert FFmpeg Graphs

Typed allowlisted filters remain Databender's default. Expert graphs provide an explicit escape hatch for installed FFmpeg audio and video filters while retaining stream targeting, direct process invocation, cancellation, timeout, output validation, and atomic publication.

Use one of two first-class filter types:

```text
expert-audio-graph:volume=0.5,aecho=0.8:0.9:20:0.2
expert-video-graph:hue=h=30,eq=contrast=1.2
```

The type fixes the target to audio or video. Expert graphs participate in normal adjacent-stage ordering, so typed and expert filters targeting the same FFmpeg domain compile into one resolved graph. `plan` prints that exact graph and marks the plan environment-dependent. Batch JSON and resumable manifests record `environment_dependent` plus each resolved graph and its explicit target.

## Inspection and Denial

Expert input is a bounded single-chain filter fragment, not an arbitrary `filter_complex` program. The parser:

- limits the UTF-8 fragment to 4096 bytes;
- recognizes comma-separated filter nodes with quoted or escaped commas;
- rejects empty nodes, malformed filter names, unterminated quotes, and escapes;
- rejects control characters, `$`, backticks, labels, semicolons, and multiple chains;
- rejects filesystem, network, pipe, data, and related protocol prefixes;
- rejects filters and options that can load external resources, execute commands, or open control sockets.

Denied resource filters include `movie`, `amovie`, `subtitles`, `ass`, `drawtext`, `lut3d`, `frei0r`, `ladspa`, `lv2`, `sendcmd`, `asendcmd`, `zmq`, and `azmq`. Denied resource options include `file`, `filename`, `fontfile`, `textfile`, `commands`, and `url`. This list is part of the supported security policy and may become stricter as FFmpeg evolves.

Every parsed filter name is queried through `ffmpeg -h filter=<name>` during pipeline preflight. Missing filters fail before candidate publication. Databender passes accepted graph text as one direct `Command` argument to `-af` or `-vf`; no shell interprets the fragment.

## Reproducibility

Expert graph results are environment-dependent because FFmpeg versions, builds, enabled filters, and platform libraries can change behavior. The complete resolved graph is still fingerprinted in batch requests and recorded in plans and reports. Changing graph text invalidates resume compatibility.

Expert stages retain the same configured process timeout and cooperative cancellation behavior as typed FFmpeg stages. Their outputs pass existing geometry, stream order, timing, metadata, full-decode, and atomic-publication validation before replacing a destination.