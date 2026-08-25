# MP3 Structural Parsing

`codecs::mp3_frames::parse` provides the structural boundary for encoded MP3 mutation. It accepts MPEG-1, MPEG-2, and MPEG-2.5 Layer III streams with indexed bitrates and sample rates.

The parser records:

- leading ID3v2 metadata, including synchsafe payload size and optional footer;
- every four-byte MPEG frame header;
- optional two-byte CRC fields;
- version- and channel-specific side-information ranges;
- decoded `main_data_begin` reservoir references;
- post-side-information main-data ranges;
- trailing ID3v1 metadata.

Frame lengths use the version-specific Layer III bitrate and sample-rate tables. Reserved versions/layers, free or reserved bitrate indexes, reserved sample rates, truncated frames, empty payloads, invalid sync, malformed ID3 sizes, and reservoir references beyond available preceding main data are rejected.

Only `Mp3Frame::main_data` ranges in frames without stored CRCs are eligible for `mp3-main-data-noise`. Header, CRC, side-information, and metadata ranges are explicit and remain byte-identical. The filter targets a bounded frame interval, selects up to its byte budget without replacement, and flips up to the intensity-derived bit count deterministically. Plans and batch dry runs report the upper-bound impact. Final validation reparses the structure, checks MP3 stream geometry and metadata, and fully decodes the candidate before atomic publication. The adapter does not classify scale-factor and Huffman bit fields within main data or tolerate candidates that fail complete decoding.