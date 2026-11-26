# Pinned upstream notice supplements

Most records are actual public upstream legal texts omitted from published crate
archives, not reconstructed notices. `source.json` binds each such supplement
to the crate name/version/license expression, published `.cargo_vcs_info.json`
commit/path, exact-commit public URL and raw SHA-256. Shared `_texts/` files avoid
duplicating identical notices. `encoding: "json-utf8"` preserves exact notices
without a final newline; hash the decoded UTF-8 bytes, not the JSON container.

`scripts/package-licenses.py` verifies those bindings offline before embedding
the notices and provenance. It never fetches upstream during packaging. The
root package license and local cached-crate notices are collected separately.
GPUI currently uses the ordinary crates.io notice collection. Its retained,
inactive vendored fallback contributes the Apache text/modification ADR only
if the local Cargo override is re-enabled.

## Authenticated standard declarations

The initial five searches below did not establish standalone upstream notice files.
Their records therefore use the explicitly different `declared-spdx` type:
exact versions, expressions and published `.crate` checksums, authenticated
against Cargo.lock, bind recognized standard grants by reference. The collector
retains actual archive manifests/README/authors and all actual source comment
notices, then supplies pinned SPDX canonical terms. Pathfinder's actual
copyright/license source headers are preserved despite its unavailable VCS SHA.
The status is `declared-standard-text`, never “upstream shipped this LICENSE.”
See [POLICY.md](POLICY.md) for the fixed fourteen-case allowlist, authentication,
placeholder omission, rejection rules and limits.

| Published crate | Declared expression | Exact VCS commit | Evidence |
| --- | --- | --- | --- |
| `convert_case 0.4.0` | `MIT` | `300d9eef9e8c970e15f324afbc3df8b6a4ebbf71` | [Complete exact-commit tree](https://api.github.com/repos/rutrum/convert-case/git/trees/300d9eef9e8c970e15f324afbc3df8b6a4ebbf71?recursive=1) has no legal files; pinned README, manifest and library-source legal-section checks supplied no actual text. |
| `hexf-parse 0.2.1` | `CC0-1.0` | `4225763d744183d720f575ae96d04161b4d08ea0` | [Complete exact-commit tree](https://api.github.com/repos/lifthrasiir/hexf/git/trees/4225763d744183d720f575ae96d04161b4d08ea0?recursive=1), README, `parse/Cargo.toml` and library sources contain no actual CC0 text. |
| `simd_helpers 0.1.0` | `MIT` | `ca1a2f84aa386d758e98f8a609d990263932fb85` | [Complete exact-commit tree](https://api.github.com/repos/lu-zero/simd_helpers/git/trees/ca1a2f84aa386d758e98f8a609d990263932fb85?recursive=1), README, manifest and library-source check supplied no actual legal text. |
| `seahash 4.1.0` | `MIT` | `94b632aeac099031c373599313d5b5f0acbbaec0` | [Original GitLab recursive tree](https://gitlab.redox-os.org/api/v4/projects/redox-os%2Fseahash/repository/tree?ref=94b632aeac099031c373599313d5b5f0acbbaec0&recursive=true&per_page=100) and [same-commit GitHub mirror](https://api.github.com/repos/redox-unofficial-mirrors/seahash/git/trees/94b632aeac099031c373599313d5b5f0acbbaec0?recursive=1) contain no legal files; pinned README and library-source checks supplied no actual text. |
| `pathfinder_geometry 0.5.1` | `MIT/Apache-2.0` | `a5e98fac00f433fe2ff4b2135383d82491b8bfc5` | Declared `servo/pathfinder` commit/tree API returns 422; pinned manifest/license raw URLs return 404. Same-commit `pcwalton/pathfinder` raw URLs also return 404. No different revision was substituted. |

The original repository-search evidence above remains for provenance; it is not
used to negate authenticated published standard grants. No owner or year is
invented when an archive provides no copyright notice. Strict collection accepts
these bounded cases and continues to reject missing/conflicting/custom or
unrecognized declarations. No compatibility conclusion, complete SBOM,
corresponding-source offer or human legal approval is asserted.

## macOS arm64 inventory

All 13 archives implicated by the native macOS QA inventory were downloaded from
their exact published `static.crates.io` URLs and SHA-256 checked against the
unchanged parent `Cargo.lock`. Eight `upstream` records recover the exact-commit
`objc2` workspace-root `LICENSE.md`: `block2 0.6.2`, `dispatch2 0.3.1`,
`objc2 0.6.5`, `objc2-encode 4.1.0`, and the four `objc2-{app-kit,core-foundation,metal,quartz-core}
0.3.2` crates. Published VCS metadata, package manifests and inherited workspace
values were checked. The identical actual notice is content-addressed under
`_texts/7f976f7e9cb2d87df7230606feb932c3f21ac0e664045a775b600046ff850c54.txt`.
It is a grant by reference, not a full standard-terms file. Its Apple SDK caveat
is retained verbatim, alongside full pinned SPDX MIT or Zlib/Apache-2.0/MIT terms
with separate provenance. This does not resolve the caveat or assert legal approval.

Five further exact standard declarations were explicitly reviewed under the
same policy after bounded research found no actual package legal text:

| Published crate | Authenticated expression | Authenticated authors | Exact source commit |
| --- | --- | --- | --- |
| `block 0.1.6` | `MIT` | Steven Sheldon | [`47178790cfc9d4a8b092051d8b413b78bd31254a`](https://github.com/SSheldon/rust-block/tree/47178790cfc9d4a8b092051d8b413b78bd31254a) |
| `leak 0.1.2` | `Apache-2.0 OR MIT` | Cody P Schafer <dev@codyps.com> | [`024bd1ccfa33aea1eff0026b0d8b676874b5e3d2`](https://github.com/jmesmon/leak/tree/024bd1ccfa33aea1eff0026b0d8b676874b5e3d2) |
| `leaky-cow 0.1.1` | `MIT / Apache-2.0` | Michael Howell <michael@notriddle.com> | [`88f4a8cd01ffc634d9f7642b97eb764a8dcd3760`](https://github.com/notriddle/rust-leaky-cow/tree/88f4a8cd01ffc634d9f7642b97eb764a8dcd3760) |
| `malloc_buf 0.0.6` | `MIT` | Steven Sheldon | [`230861bf873ec72a644ae5a1b39fdf0b3a1ce654`](https://github.com/SSheldon/malloc_buf/tree/230861bf873ec72a644ae5a1b39fdf0b3a1ce654) |
| `objc_exception 0.1.2` | `MIT` | Steven Sheldon | [`c86ad3a52984461fc5c63980d12e8ceed847854c`](https://github.com/SSheldon/rust-objc-exception/tree/c86ad3a52984461fc5c63980d12e8ceed847854c) |

The first four legacy archives have no `.cargo_vcs_info.json`; every published
member byte-matches the recorded source commit. The fifth has authenticated VCS
metadata. Each `source.json` retains archive checksum, exact tree URL, binding
method, inspected scope and absence evidence. No package copyright/license
header was found in these five archives; output provenance records zero actual
notices and retains the manifests/authors/README where present. The unrelated
CC0 CI-script header in `leak` and a newer `malloc_buf` LICENSE are not substituted.

Validation from parent `405a85178006d8058dc3ed6acfde73be3dd7ba41` on Linux:

- `python3 scripts/package-licenses-test.py`: all 20 tests pass (the original 14
  plus legacy-expression, future-version and upstream-supplement regressions).
- Offline `--locked --filter-platform aarch64-apple-darwin` strict collection:
  **618 packages / 1157 notice entries / 0 warnings**, including ten explicitly
  authenticated standard-declaration packages.
- Offline strict `x86_64-unknown-linux-gnu` collection remains
  **670 packages / 1222 notice entries / 0 warnings**, including the original five
  standard-declaration packages. Existing Linux override files remain intact.

Dependencies were fetched before the offline checks; the root manifests/lock
were not modified. Both collection commands used `--binary /usr/bin/true` as an
explicit inventory-only placeholder, not a Mac executable/build attestation.
No native Mac compilation, DMG smoke, signing or production legal audit was run
for this licensing change.

## RuTracker / scraper 0.20 notice supplements

The local RuTracker dependency closure adds eight missing-notice cases. The
`phf` family uses recovered, unmodified workspace-root `LICENSE` bytes from
the repository named in the published manifests:

| Published crate | Published VCS commit | Exact upstream notice |
| --- | --- | --- |
| `phf 0.10.1` | `bed01538ae576876f11189d541875d228acef9e8` | [MIT LICENSE](https://raw.githubusercontent.com/sfackler/rust-phf/bed01538ae576876f11189d541875d228acef9e8/LICENSE) |
| `phf_codegen 0.10.0`, `phf_generator 0.10.0`, `phf_shared 0.10.0` | `3ea14b2166553ad6e7b9afe7244144f5d661b6c6` | [MIT LICENSE](https://raw.githubusercontent.com/sfackler/rust-phf/3ea14b2166553ad6e7b9afe7244144f5d661b6c6/LICENSE) |

Each published archive was SHA-256 checked against Cargo.lock. Each package's
manifest at its exact source commit confirms its name/version/MIT declaration.
Both complete exact-commit trees contain the same actual license bytes, stored
under `_texts/d8870d5e2ab20fc50915b711c5aa7588131c9abcc1bcb980da1efd7486522a46.txt`.
The three 0.10.0 archives omit `path_in_vcs`; their records deliberately retain
the empty published value rather than replacing it with an inferred path.

Two legacy archives have no published VCS metadata and no standalone legal
files, so they use the existing exact-archive `declared-spdx` mechanism:

| Published crate | Exact declaration | Authenticated source |
| --- | --- | --- |
| `fxhash 0.2.1` | `Apache-2.0/MIT` | [Complete tree](https://api.github.com/repos/cbreeden/fxhash/git/trees/537570af68de22d2f4bbdd5bee5d6e07268e6f42?recursive=1); all original archive members match, with `Cargo.toml.orig` mapped to upstream `Cargo.toml`. The generated normalized manifest agrees on the grant and is authenticated by the archive hash. |
| `mac 0.1.1` | `MIT/Apache-2.0` | [Complete release tree](https://api.github.com/repos/reem/rust-mac/git/trees/66afc663b68a65633ea165c742b5a9c6734581c2?recursive=1); every published member byte-matches this exact release commit. |

The actual 2015 Rust Project Developers source notice in `fxhash/lib.rs` is
retained verbatim. `mac`'s actual README grant and authors are retained as
published metadata; no copyright notice was found or invented. Both receive
full pinned canonical MIT/Apache terms, explicitly labeled as standard terms
rather than recovered repository LICENSE files. Future versions and changed
literal expressions are not accepted automatically.

`selectors 0.25.0` and `servo_arc 0.3.0` also use exact-archive declarations.
Their published VCS commits are respectively
`91ae9eb83291f249c4455909834732a6a806c799` and
`518560b4e06ad9aa1a407d9f27293a2d20dcd804`, with paths
`servo/components/selectors` and `servo/components/servo_arc`. Exact-commit
queries fail in both the declared `servo/servo` and `mozilla/gecko-dev` sources
(GitHub API 422/raw 404); Mercurial mozilla-central does not recognize those
revisions either. [Servo issue 30455](https://github.com/servo/servo/issues/30455)
corroborates the unavailable `servo_arc` revision and possible Gecko publication,
not a new license. No current Gecko/Stylo text or unrelated whole-repository
license was substituted.

The checksummed archives directly authenticate both Cargo manifests, authors,
README files, all MPL-2.0 source notices in `selectors`, and the actual
2012-2014 Rust Project Developers Apache/MIT header in `servo_arc`.
Canonical MPL-2.0 terms come from the existing SPDX v3.26.0 commit
`8a04f0303d95c4f932210b118588c662665bd383`, raw SHA-256
`66a3107d5ad6a058aab753eaac2047ccb2ed0e39465dd0fe5844da3e300d5172`.
Those complete bytes, including the source's trailing space in definition 1.7,
are preserved rather than reformatted. The MPL text is copied verbatim; its
exhibits are instructions, not an assertion that Exhibit B applies to `selectors`.
Both cases remain `declared-standard-text`, **not** a recovered upstream LICENSE
or human legal clearance. Release source availability and notice obligations
(including MPL §3.2), compatibility and other distribution obligations remain
for human review.

Validation for this eight-case restoration:

- `python3 scripts/package-licenses-test.py`: **29 tests passed**, including
  exact phf VCS/hash bindings, mismatched provenance rejection, the four newly
  reviewed standard grants, changed-expression/future-version rejection, and
  the real canonical MPL hash.
- Native `python3 scripts/package-licenses.py --binary <scratch-placeholder>
  --output <new-dir> --strict`: **707 packages / 1304 notice entries /
  0 warnings**, target `x86_64-unknown-linux-gnu`, status
  `texts-collected-not-audit`; nine declared-standard packages in this target.
  Every embedded notice hash was rechecked. Actual source notice counts are
  fxhash: 1, mac: 0, selectors: 12, servo_arc: 1.
- The isolated checkout has no built application. The temporary input explicitly
  says **NON-DISTRIBUTABLE / NOT an executable**. This checks the exact native
  offline metadata/notice pipeline, not a real executable or package build;
  the parent must rerun strict collection against its actual `kadr-app`.

No dependency changes, packages, uploads, signing or commits were made.
