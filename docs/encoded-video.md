# Encoded Video Packet Adapters

The low-level `codecs::encoded_video::mutate_packet` API applies deterministic, length-preserving mutations to one demuxed video packet. Callers provide the probed codec name, packet bytes, optional NAL length-field width, byte budget, intensity, and seed. It returns eligible and changed byte counts.

The initial adapters use conservative mutable regions:

- H.264 mutates only VCL NAL types 1 and 5. SPS, PPS, SEI, access-unit delimiters, length fields, NAL headers, and the first 16 VCL payload bytes remain unchanged.
- H.265 mutates only VCL NAL types 0 through 31. VPS, SPS, PPS, other non-VCL units, length fields, two-byte NAL headers, and the first 16 VCL payload bytes remain unchanged.
- VP8 preserves the frame tag, keyframe start code and dimensions, plus 16 following header bytes.
- VP9 validates the frame marker and preserves the first 16 bytes.
- AV1 mutates only explicit-size tile-group OBU payloads. Sequence headers, metadata, frame headers, temporal delimiters, OBU headers, extension headers, and size fields remain unchanged.

Malformed lengths, headers, start codes, reserved bits, and unsupported codecs are rejected before mutation. Selection is without replacement and deterministic for identical bytes, controls, and seed.

MP4 exposes the same adapters through `video-packet-noise:byte_budget=COUNT,start_packet=INDEX,packet_count=COUNT,frame_type=all|key|delta,intensity=FRACTION,max_frame_loss=COUNT`. Defaults are an eight-byte budget, packet zero onward, all frame types, intensity 0.125, and no tolerated frame loss. A zero packet count selects the remainder of the stream. Repeated `--video-stream` options select streams, and plans and dry-run reports include the bounded mutation impact.

The MP4 and Matroska executors copy the source container and overwrite only equal-length packet payloads. MP4 positions must directly match ffprobe's packet SHA-256. Matroska positions include block framing, so a bounded EBML parser locates unlaced `SimpleBlock` and `Block` payloads and resolves selected packets by forward size and SHA-256 match. Packet ranges must be non-overlapping. This leaves sample tables, cues, indexes, timestamps, metadata, block headers, and every byte outside selected payloads unchanged. Candidate validation checks stream topology, codec, dimensions, frame rate, bounded frame loss, audio properties, metadata, Matroska auxiliary topology, and a complete FFmpeg decode before atomic publication.

Packet stages cannot currently be mixed with decoded audio or video stages. Laced Matroska blocks are not writable targets; selected packets must resolve to exact unlaced payloads or execution fails before publication.