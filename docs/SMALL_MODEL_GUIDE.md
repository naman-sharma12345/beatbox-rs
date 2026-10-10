# Driving beatbox with a small model

beatbox is MCP first: every button in the studio is a tool an AI can call, and
the tools are shaped so a small local model (3-8B) can drive the whole DAW.
This page is the short version; `get_guide` is the long one.

## Run it

```sh
beatbox mcp                      # MCP over stdio (Claude Desktop, Cursor, any MCP client)
beatbox mcp --connect http://127.0.0.1:7878   # drive a running studio window live
beatbox call make_beat '{"prompt":"dark 90 bpm desi hip-hop with a beat switch"}'
```

## Three calls are enough

| you want | call |
|---|---|
| a finished beat from words | `make_beat {prompt, lyrics?, vocal?}` |
| a song from a voice memo / speech | `produce_song {path, mode: rap or sing}` |
| the file | `export_audio {path: "song.mp3"}` |

Everything else is for changing details.

## Finding a tool (never guess names)

1. `list_tools` returns every tool grouped by category, one line each.
   `list_tools {category: "vocal"}` or `{search: "reverb"}` narrows it.
2. `suggest_tools {goal: "make the vocal louder"}` matches plain words to tools
   and returns an example call for the best one.
3. `describe_tool {name}` gives every argument (type, range, default,
   required), a ready-to-send `example`, the usual `next` tools and related
   tools. Copy the example, change the values, send it.

A wrong tool name is answered with `Did you mean: ...` and the three closest
names. A wrong argument name is refused with the list of valid ones and
nothing is changed.

## Reading results

- Every result is JSON. Changing tools return `changes` (what moved, in
  words) and `revision`.
- Many results carry `next`: the tools usually called after this one. Follow
  them when unsure.
- `get_project_summary` is the project on one screen with its own `next`
  list. Prefer it over `get_project {detail: "full"}`, which is large.

## Safe experimenting

- `undo` / `redo` step through every change (100 deep).
- `snapshot {name}` saves a version; `restore_snapshot`, `compare_variants`
  and `ab_compare` compare them.
- `batch {calls: [{tool, args}, ...]}` runs several edits as one step; if one
  fails, none apply (atomic by default).

## Units

- Time is 16th-note steps: 16 per bar, 4 per beat.
- Pitches: MIDI numbers or names (`C4` = 60, `F#2`).
- Levels in dB (`volume_db: -6`), pan -1..1, BPM 40..300.
- Omit `seed` for a fresh result each call; pass the returned seed to repeat it.

## A complete small-model session

```json
{"tool": "make_beat", "args": {"prompt": "sad punjabi r&b, 84 bpm, piano and flute"}}
{"tool": "critique_track", "args": {}}
{"tool": "set_mixer", "args": {"track": "lead", "volume_db": -3}}
{"tool": "export_audio", "args": {"path": "sad_rnb.mp3"}}
```
