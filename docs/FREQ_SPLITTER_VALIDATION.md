# freq_splitter validation (behavior change)

`add_effect` and `tweak_effect` now run `fx_extra::check_freq_splitter`
through `Effect::validate`. This CHANGES inputs that used to work.

## Rejected now, accepted before

| Input | Before | Now |
|---|---|---|
| `low_hz` or `high_hz` below 20 or above 20000 (incl. negative, over Nyquist) | accepted, clamped silently inside the filter design | rejected, error names the field |
| `low_hz` or `high_hz` NaN or infinite | accepted (NaN low gave silence) | rejected |
| `taps` 1 or 3 (also 5..31) | accepted; 1 and 3 gave a silent mid band | rejected (minimum 33) |
| `taps` above 65537 | accepted; cost grows with taps, above ~1M the output is silent for short clips, u32::MAX asks for a ~16 GB buffer | rejected (maximum 65537) |
| `low_hz` equal to `high_hz` | accepted; silent mid band | rejected |
| `gain_db` outside -60..24 or non-finite | accepted | rejected |
| `mix` outside 0..1 or non-finite | `mix` was clamped, others passed | rejected |

## Unchanged

- Inverted `low_hz` > `high_hz` is still valid; it is swapped as before.
- Even `taps` still round up to the next odd value (514 gives the same
  output as 515). Odd values in 33..=65537 give the same output as before.
- Even-`taps` rounding at the new lower edge, measured on a copy of the
  exact code (existing behaviour, not changed here): 34 becomes 35 (centre
  index 17) and 64 becomes 65 (centre index 32), and the output is
  identical to asking for 35 and 65 directly. The impulse response is
  symmetric to float rounding only. Largest mirror difference (tap i against
  tap N-1-i): 34 -> 1.9e-8 low band, 1.5e-8 high band; 64 -> 9.3e-9 low,
  5.6e-9 high; the default 513 -> 9.3e-10 low, 3.7e-9 high. DC gain is
  1.000000.
- Known debt: the response is not bit-exact symmetric (a handful of mirror
  pairs differ in the last float bit, for example 26/35 at 34 taps and
  392/400 at 513). Writing the mirrored half would fix it but changes
  output bits, so it is left as is.
- The DSP code (`freq_splitter`, the streamed arm) is untouched; only the
  check was added. No golden render, preset, playbook or doc used
  freq_splitter before this unit.

## How a rejection behaves

- `validate` returns `Err(String)` (the repo's existing validate error
  type, not a new typed enum). Nothing panics.
- The taps bound is checked first, before any buffer is created, so a huge
  value (u32::MAX) fails without allocating anything sized by it. The
  error message is a fixed-size string.
- A rejected `tweak_effect` now leaves the effect unchanged (earlier it
  assigned the new values before validating).

## Not covered

Validation runs through the tool path (`add_effect`, `tweak_effect`,
chain loading in `tools_chain`). A project file or JSON deserialized
directly into an `Effect` and processed without those paths is not
checked; a bound inside `freq_splitter` itself would be a separate unit.
