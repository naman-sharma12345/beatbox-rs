# Driving beatbox with a small model

beatbox is MCP first: every button in the studio is a tool an AI can call, and
the tools are shaped so a small local model (3-8B) can drive the whole DAW.
This page is the short version; `get_guide` is the long one.

**The tool list is categories.** `tools/list` shows about 20 tools (`project`,
`pattern`, `notes`, `instrument`, `sample`, `arrange`, `mixer`, `effects`,
`vocals`, `analyze`, `generate`, `export`, ...). Call one with
`{"action": "<name>", "params": {...}}`; the engine tool names on this page are
those actions (`set_tempo` is `project` / `set_tempo`). `{"action": "help"}` on
any tool lists its actions. Old names still work when called directly.

## Run it

```sh
beatbox mcp                      # MCP over stdio (Claude Desktop, Cursor, any MCP client)
beatbox mcp --tools compact      # advertise ~25 tools instead of all of them (small models)
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

## Compact tool list (staged schemas)

The full registry is large. A small model should not carry every schema in
its context, so the server can advertise a short list instead:

```sh
beatbox mcp --tools compact        # or BEATBOX_MCP_TOOLS=compact
```

`tools/list` then holds about 25 tools: discovery (`list_tool_groups`,
`search_tools`, `describe_tool`, `call_tool`, `get_guide`), the one-call
producers, the everyday edits (tracks, notes, mixer, effects, tempo, key,
ids), render/export and undo/redo. Everything else is still there and runs
the same way; it is found and called in three steps, and only the schema of
the tool about to be called is ever loaded:

1. `list_tool_groups`: every group, what it is for, how many tools, a few
   to start with (a few hundred tokens).
2. `search_tools {query: "split clip"}`: ranked matches, one line each, with
   the required arguments and whether the tool changes the project.
3. `describe_tool {name}`: the full JSON schema (`input_schema`), every
   argument explained and a ready-to-send `example`.

Then `call_tool {name, arguments}` (or the tool directly, if the client
allows calling unlisted tools). Same validation, undo and results.

## Finding a tool (never guess names)

1. `list_tool_groups` for the map, `search_tools {query}` for a goal in
   plain words, or `list_tools` for every tool grouped by category, one line
   each (`list_tools {category: "vocal"}` or `{search: "reverb"}`).
2. `suggest_tools {goal: "make the vocal louder"}` matches plain words to tools
   and returns an example call for the best one.
3. `describe_tool {name}` gives every argument (type, range, default,
   required), the full `input_schema`, a ready-to-send `example`, the usual
   `next` tools and related tools. Copy the example, change the values, send it.

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

## Stable ids

Every track, bus, pattern, note, clip and automation lane has an id
(`trk3`, `n41`, `clip7`) that survives renames, undo and save/load. Any
argument that takes a track, bus or pattern name also takes its id; note
tools take `ids`; clip tools take `id`. `list_ids` and `lookup_id` show them.

## Listening is not measuring

`ab_listen` writes loudness-matched A/B clips for a listener and never names
a winner. Loudness, spectrum and DNSMOS scores describe level and damage,
not taste: when two versions differ musically, a listener decides (a
person, or a model that can hear the attached audio).
