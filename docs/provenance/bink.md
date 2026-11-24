# Bink decoder provenance

Status: **derived from FFmpeg (LGPL-2.1-or-later) as far as checked. Not
cleared for distribution under Apache-2.0.**

`crates/bink` was added in commit `23f62f3` with no record of its sources. The
original authoring session is lost. This note records what a later comparison
against FFmpeg `n7.1` (`libavcodec/`) found. It does not establish how the code
was written; it establishes how closely it matches.

Reproduce with `scripts/bink-ffmpeg-compare.py` (inputs and pinned tag in its
docstring).

## Tables

Compared as parsed value sequences against FFmpeg `n7.1`:

| crate constant | FFmpeg source | result |
| --- | --- | --- |
| `SCAN` | `binkdata.h` `bink_scan` | identical |
| `TREE_BITS`, `TREE_LENS` | `binkdata.h` `bink_tree_bits`, `bink_tree_lens` | identical |
| `PATTERNS` | `binkdata.h` `bink_patterns` | identical |
| `INTRA_QUANT`, `INTER_QUANT` | `binkdata.h` `bink_intra_quant`, `bink_inter_quant` | identical |
| `CRITICAL_FREQS` | `wma_freqs.c` `ff_wma_critical_freqs` | identical |
| `RLE_LENS` (audio) | `binkaudio.c` `rle_length_tab` | identical |

Format constants have to match any compatible decoder, so equality alone is
expected. It does mean no independent derivation can be shown, and the former
`tables.rs` header ("Generated from the published format tables") named no
generator, no inputs and no source for matrix corrections. None exist in the
repository history.

## Code

- `idct8` (`video.rs`) is a term-for-term transliteration of `IDCT_TRANSFORM`
  in `binkdsp.c`: the same constants (2896, 2217, 3784, -5352), `>> 11`
  multiply, intermediate names `a0..a7`/`b0..b4`, output expressions in the same
  order, and the `(x + 0x7f) >> 8` row rounding.
- `dct_coeffs` and `residue` (`video.rs`) follow `read_dct_coeffs` and
  `read_residue` in `bink.c`: the same 128-entry `coef`/`mode` work lists,
  the same initial entries, and the same control flow, including the
  case-0-falls-into-case-2 behaviour, written out explicitly in Rust.
- `Audio::coefficients`, band setup and `RLE_LENS` use (`audio.rs`) follow
  `decode_block` and `decode_init` in `binkaudio.c`.

The Rust is restructured (no macros, `Result` instead of error codes, Rust
bit reader), but the logic is not independent.

## Licence consequence

FFmpeg's Bink files are LGPL-2.1-or-later. Code adapted from them cannot be
relicensed as Apache-2.0. `NOTICE` currently mentions FFmpeg only for the WMA2
decoder, and its "no FFmpeg source ... is distributed" statement does not
cover this crate.

## Options

1. Replace the crate with a clean-room implementation written from the format
   description by someone who has not read FFmpeg's Bink code, with its
   sources and any table generator committed.
2. Keep the crate, mark it LGPL-2.1-or-later with FFmpeg attribution, and
   isolate it so the licence boundary is clear.
3. Remove Bink playback from the public tree.

Until one is chosen, do not describe this crate as independently written or as
Apache-2.0.
