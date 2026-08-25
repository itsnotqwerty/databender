# Usability Sessions

This protocol covers the final v0.5 release gate for pipeline editing and batch recovery. Record real participant observations; automated tests and maintainer walkthroughs do not count as usability sessions.

## Participants and Setup

Run at least three sessions with people who are comfortable using a terminal but did not implement the tested workflow. Use a clean checkout and representative PNG inputs. Record the Databender revision, operating system, terminal, participant experience level, completion time, errors, prompts requested, and brief comments. Do not record names or terminal contents unrelated to the study.

Build the binary and prepare three inputs before each session:

```bash
cargo build
mkdir -p usability-input usability-output
cp path/to/three/images/*.png usability-input/
```

The moderator may explain that Databender transforms media, but must not reveal commands or key bindings after a task begins. A participant may use `--help`, the README, visible TUI text, and the `?` controls guide. Stop a task after ten minutes or when the participant says they cannot proceed.

## Task A: Edit and Run a Pipeline

Prompt: "Open the terminal interface for `usability-input`, send outputs to `usability-output`, add `invert` followed by `channel-shift:pixels=2`, correct any mistake you encounter, and process the files."

Expected entry command:

```bash
target/debug/databender tui usability-input --output-dir usability-output
```

Observe whether the participant can:

1. Discover the bottom controls hint or `?` guide, use `p` to enter each filter, and understand immediate preflight feedback.
2. Recognize filter order, select an incorrect filter with Left/Right, and edit it with `e` or remove it with `d`.
3. Start processing with `s` and identify queue progress or failure.
4. Exit without assistance using `q` or Escape.

Success requires correctly ordered filters, at least one published output, and no moderator instruction. Record partial success when output is produced only after one neutral prompt such as "What information is visible on screen?"

## Task B: Recover a Batch

Before the task, run:

```bash
rm -rf usability-output usability-manifest.json
target/debug/databender batch usability-input \
  --output-dir usability-output \
  --seed 42 \
  --filter invert \
  --manifest usability-manifest.json
missing_output=$(find usability-output -maxdepth 1 -type f -print -quit)
rm -- "$missing_output"
```

Prompt: "One output from this batch was lost. Recover only the missing work using the existing manifest, then explain which files were reused and which were processed again."

Success requires a compatible resume and correct interpretation of `resumed` versus `ok`:

```bash
target/debug/databender batch usability-input \
  --output-dir usability-output \
  --seed 42 \
  --filter invert \
  --resume usability-manifest.json
```

After successful recovery, ask the participant to change the filter to `invert,brightness:delta=10` while reusing the manifest. Record whether the fingerprint rejection explains why changed work cannot be resumed.

## Results and Release Decision

Append one row per participant:

| Session | Revision | Platform | Experience | Pipeline result/time | Recovery result/time | Prompts | Key observations |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Example only | `0000000` | Linux / terminal | Regular CLI user | Success / 3m | Partial / 6m | 1 | Did not distinguish `resumed` from `ok` |

Classify findings as:

- **Blocking:** prevents output or recovery without moderator instruction, risks overwriting unintended files, or leaves queue state unclear.
- **Major:** task completes only after repeated trial and error or documentation lookup caused by missing interface feedback.
- **Minor:** hesitation or terminology confusion that does not change the outcome.

Complete the roadmap gate only after all sessions are recorded, no blocking findings remain, and major findings are either fixed and rechecked with a participant or explicitly accepted in release notes. Summarize resulting changes below the table and retain the anonymized rows as release evidence.