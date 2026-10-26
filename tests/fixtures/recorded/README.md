# Recorded regression fixtures

Run `python3 scripts/prepare_test_fixtures.py` before the recorded regression tests.
Affected tests prepare these once per test process automatically, so both cargo test modes work on a clean runner with Python 3. The script prepares five project
fixtures and downloads only their existing VCSL drum and Yamaha piano assets.

The project JSON files preserve the development regression inputs. Their only
normalization is replacing machine-specific sample paths with asset hash names.
The preparation script resolves these to the local cache, then writes the input
projects to the temporary paths expected by the current tests. Generated files
and downloaded WAV files are not committed.

Assets come from https://github.com/sgossner/VCSL at commit
c1ea7bcc3c7309650ab0da9d15c9cd1fbc4a4c7e. Every download must match its manifest
SHA-256 and byte count. These are the already selected VCSL drums and Yamaha
upright piano, not new sample packs. VCSL is CC0; upstream license:
https://github.com/sgossner/VCSL/blob/c1ea7bcc3c7309650ab0da9d15c9cd1fbc4a4c7e/LICENSE
