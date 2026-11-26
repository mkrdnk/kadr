# Decision record: authenticated standard-license declarations

## Decision and scope

Prefer actual upstream notice texts: keep local crate notices and exact-commit
workspace supplements verbatim. A published Cargo declaration of a recognized
standard license is also a grant by reference, not evidence that no grant exists
merely because the crate archive omitted a standalone LICENSE file.

The initial collector required a recovered notice file for every third-party
package. That text-only rule unnecessarily blocked authenticated published
standard declarations. A separate `declared-spdx` provenance type now covers
**only these exact versions and expressions**:

| Package | Authenticated declaration | Canonical terms supplied |
| --- | --- | --- |
| `convert_case 0.4.0` | `MIT` | MIT |
| `hexf-parse 0.2.1` | `CC0-1.0` | CC0-1.0 |
| `simd_helpers 0.1.0` | `MIT` | MIT |
| `seahash 4.1.0` | `MIT` | MIT |
| `pathfinder_geometry 0.5.1` | `MIT/Apache-2.0` | MIT and Apache-2.0, preserving Cargo's legacy alternative-grant declaration |
| `block 0.1.6` | `MIT` | MIT |
| `leak 0.1.2` | `Apache-2.0 OR MIT` | Apache-2.0 and MIT |
| `leaky-cow 0.1.1` | `MIT / Apache-2.0` | MIT and Apache-2.0, preserving this exact legacy alternative-grant declaration |
| `malloc_buf 0.0.6` | `MIT` | MIT |
| `objc_exception 0.1.2` | `MIT` | MIT |
| `fxhash 0.2.1` | `Apache-2.0/MIT` | Apache-2.0 and MIT, preserving this exact legacy alternative-grant declaration |
| `mac 0.1.1` | `MIT/Apache-2.0` | MIT and Apache-2.0, preserving this exact legacy alternative-grant declaration |
| `selectors 0.25.0` | `MPL-2.0` | MPL-2.0 |
| `servo_arc 0.3.0` | `MIT/Apache-2.0` | MIT and Apache-2.0, preserving this exact legacy alternative-grant declaration |

This is a reviewed **fourteen-case** allowlist, not an automatic fallback for arbitrary SPDX-like
strings. Unknown/custom expressions, `NOASSERTION`, declaration conflicts,
unexpected versions, missing authenticated archives, and checksum mismatches
still fail collection. Expanding the allowlist requires an explicit review.

The five rows from `block` through `objc_exception` were reviewed for the macOS inventory after
checksum-authenticated archives and their exact source trees supplied no actual
package legal text. They have standard grants with no conflicting/custom terms,
so the same existing grant-by-reference policy applies. Four archives predate
`.cargo_vcs_info.json`; every published member byte-matches the source commit
recorded in `source_evidence`. `objc_exception` has published VCS metadata.
Their complete declarations/authors and absence of copyright/source-header notices
are retained, not replaced with inferred ownership. No generic normalization of
legacy expressions is permitted. This engineering collection decision is not
human legal approval.

The two RuTracker/scraper legacy cases, `fxhash 0.2.1` and `mac 0.1.1`,
were reviewed under the same bounded rule. Neither published archive includes
VCS metadata or a standalone legal file. Exact upstream release trees were
inspected: all original `fxhash` members match commit
`537570af68de22d2f4bbdd5bee5d6e07268e6f42` (with the generated manifest separately
authenticated by the archive), and all `mac` members match release commit
`66afc663b68a65633ea165c742b5a9c6734581c2`. Their exact standard grants and
archive SHA-256 values are pinned in separate records. The real Rust Project
copyright/license header in `fxhash/lib.rs` is preserved; `mac` supplies no
copyright notice and none is inferred from its author metadata.

The other two scraper cases, `selectors 0.25.0` and `servo_arc 0.3.0`, have
published VCS metadata but their exact commits are unavailable in both the
declared Servo repository and Gecko's Git/Mercurial sources. Like Pathfinder,
they are authenticated by the locked published archive rather than a
substituted newer revision. Their original/normalized manifests agree on the
standard grant. All actual MPL source notices in `selectors` and the original
Rust Project Developers Apache/MIT header in `servo_arc` are preserved.
The recognized MPL-2.0 terms are pinned to the same SPDX commit as the other
canonical assets and copied verbatim, including their instructional exhibits.
This grants no legal clearance: MPL source-availability/notice obligations and
all other release compatibility/source obligations still require human review.

## Authentication and preservation

For each allowlisted case the collector:

1. Requires the checked-in crate/version/expression record to match Cargo's
   resolved metadata and the corresponding registry entry/checksum in Cargo.lock.
2. Reads the already-cached `.crate` archive, verifies its complete SHA-256
   against both sources, and inspects it without executing or unpacking code.
3. Checks the published normalized and original Cargo manifests agree on the
   package/version/license, rejects custom license-file ambiguity, and retains
   the actual published metadata, README files, author declarations and paths.
4. Preserves actual legal files and source comment notices with source paths,
   line positions and archive-member/excerpt hashes. Pathfinder's 2019/2020
   copyright and MIT/Apache source headers are copied verbatim. If an archive
   supplies no copyright notice, the report says so; no owner or year is invented.
5. Supplies canonical terms from SPDX license-list-data v3.26.0 at exact commit
   `8a04f0303d95c4f932210b118588c662665bd383`, checking the recorded raw SHA-256.
   Canonical instructional copyright-template lines are omitted from embedded
   terms rather than presented as package ownership claims. This transformation
   and both source/output hashes are recorded; no permission clause is rewritten.

For Pathfinder, the unavailable VCS revision is not an authentication failure:
the published archive checksum authenticates the manifest and actual source
headers directly. It is not represented as a recovered repository LICENSE.

Everything is offline at package time. Missing archive/source cache entries do
not cause network access or a silent relaxation. Repository assets preserve the
complete raw SPDX texts and pinned provenance for reproduction.

## Actual upstream notices incorporating standard terms

Eight exact macOS crate versions (`block2 0.6.2`, `dispatch2 0.3.1`,
`objc2 0.6.5`, `objc2-encode 4.1.0`, and `objc2-{app-kit,core-foundation,metal,quartz-core}
0.3.2`) have a recoverable root `LICENSE.md` in their published VCS commit.
These remain actual `upstream` records, **not** additional declared-SPDX fallbacks.
The notice grants MIT or Zlib/Apache-2.0/MIT by reference and includes an Apple
SDK licensing caveat. Preserve it verbatim, including that caveat, and supplement
it with full applicable canonical terms using the same verified SPDX helper.
The fixed eight-case mapping requires the exact package/version/expression and
term IDs; it cannot supply a notice for another missing-text package.

Canonical terms have distinct provenance, source/output hashes and explicit
placeholder-omission transforms, not the upstream notice's provenance. The Zlib
asset is pinned to the same SPDX commit as the existing assets; it has no
copyright template and is copied verbatim. No upstream copyright is invented.

## Status and limits

Each affected package is labeled `declared-standard-text`, not “upstream shipped
this LICENSE” or “legal counsel approved.” Strict notice collection accepts the
authenticated standard grant plus canonical terms and all actual found notices.
The overall `texts-collected-not-audit` status remains explicitly **not** a full
linked-code SBOM, compatibility audit, source-offer assessment or production
distribution approval.

Default package/release jobs remain strict; the explicit unsigned local QA mode
is not needed to accept these fourteen standard grants. It still cannot turn a
conflicting or unauthenticated declaration into a recognized grant. Signing,
Apple notarization, and production legal review remain separate external steps.
