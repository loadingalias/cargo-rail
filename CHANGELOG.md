# Changelog

## [0.7.3](https://github.com/loadingalias/cargo-rail/compare/v0.7.2...v0.7.3) - 2025-12-16

### 📦 Other Changes

- cargo-rail: fixing the release pipe; Issues #6 & 7 are addressed. added the improved macros for cleaner outputs w/o the log/tracing. [#6](https://github.com/loadingalias/cargo-rail/pull/6) ([5647d6b](https://github.com/loadingalias/cargo-rail/commit/5647d6b2dd51d12b7bbb0499ef5419551b284e84))



## [0.7.2](https://github.com/loadingalias/cargo-rail/compare/v0.7.1...v0.7.2) - 2025-12-15

### 🔧 Chores

- add pre-push hook and fix formatting [#5](https://github.com/loadingalias/cargo-rail/pull/5) ([4919985](https://github.com/loadingalias/cargo-rail/commit/4919985c086cb7bd0c48d049c7c54b0da7c75e77))



## [0.7.1](https://github.com/loadingalias/cargo-rail/compare/v0.7.0...v0.7.1) - 2025-12-15

### 👷 CI

- run workflow on pull requests ([e3fcf55](https://github.com/loadingalias/cargo-rail/commit/e3fcf558cdea1f5cf51640add66bddfa4ac588d2))

### 📦 Other Changes

- add configurable dependency sorting (#5) [#5](https://github.com/loadingalias/cargo-rail/pull/5) ([f055618](https://github.com/loadingalias/cargo-rail/commit/f05561864f4dd73f471524e20a8cdad3de619507))



## [0.7.0](https://github.com/loadingalias/cargo-rail/compare/v0.6.0...v0.7.0) - 2025-12-14

### 📦 Other Changes

- cargo-rail: fixing the keywords in the package metadata ([1a111b9](https://github.com/loadingalias/cargo-rail/commit/1a111b9caf518f84ebd2faddc5d8e0f251cf2182))



## [0.6.0](https://github.com/loadingalias/cargo-rail/compare/v0.5.3...v0.6.0) - 2025-12-14

### 📦 Other Changes

- cargo-rail: cleaning docs and fixing the broken generation script for the configs. cleaning and improving the readme; updating the docs for the last few commits. ([849273e](https://github.com/loadingalias/cargo-rail/commit/849273eb597372bf9af30ae5cd086c69fd3e3c71))
- cargo-rail: cleaning up the CLI and config outputs/comments ([9783fca](https://github.com/loadingalias/cargo-rail/commit/9783fca7894f2a33e7ab50a89dbe066c3f9834a0))
- cargo-rail: major dx, perf, and reliability updated. ([19b48c4](https://github.com/loadingalias/cargo-rail/commit/19b48c401165b1567893e093cf13ccd4fa94aebc))
- cargo-rail: cleaning up the codebase; CICD. ([7ff3906](https://github.com/loadingalias/cargo-rail/commit/7ff3906fc7062786094049b03a04a9876198227a))
- cargo-rail: added 'split/sync' safety rails; defrred 'git centralization'; removed the four unwrap()/expect() calls from the production code; fixed the 'process::exit()' in the lib code. ([6b8faa3](https://github.com/loadingalias/cargo-rail/commit/6b8faa3f5ef7b41b2d4a773721d4d6c71f5e5c49))
- cargo-rail: feat(split/sync): add safety rails with --allow-dirty and --yes flags ([70d213a](https://github.com/loadingalias/cargo-rail/commit/70d213a6a37d41fdb26476640db6d6f3d514cbb2))
- cargo-rail: feat: safety and correctness improvements ([4edda04](https://github.com/loadingalias/cargo-rail/commit/4edda04c5666788e3a489870b0b3685fc68f9eeb))



## [0.5.3](https://github.com/loadingalias/cargo-rail/compare/v0.5.2...v0.5.3) - 2025-12-12

### 🐛 Bug Fixes

- lower MSRV from 1.92.0 to 1.91.0 ([8f6a475](https://github.com/loadingalias/cargo-rail/commit/8f6a475022e755ee66e9e8217eed21f2b9d71f3a))

### 📦 Other Changes

- cargo-rail: fixing the schema/config for the 'undeclared features; resolution/fixing; added tests for it ([bc09254](https://github.com/loadingalias/cargo-rail/commit/bc0925406f907d9d1d0d5f6c2f8b9ba776ef8748))



## [0.5.2](https://github.com/loadingalias/cargo-rail/compare/v0.5.1...v0.5.2) - 2025-12-12

### 📦 Other Changes

- cargo-rail: bumping Rust version to 1.92.0 stable ([ed566c1](https://github.com/loadingalias/cargo-rail/commit/ed566c11b16b97eb65aded9befde6117e684c54d))



## [0.5.1](https://github.com/loadingalias/cargo-rail/compare/v0.5.0...v0.5.1) - 2025-12-12

### 📦 Other Changes

- cargo-rail: fixing the schema issue; cleaning petgraph features manually ([aabd8a5](https://github.com/loadingalias/cargo-rail/commit/aabd8a5289e6e4ef8d30e0a59e6c7c95984e7027))



## [0.5.0](https://github.com/loadingalias/cargo-rail/compare/v0.4.2...v0.5.0) - 2025-12-12

### 📦 Other Changes

- cargo-rail: meilisearch PR revealed a bug in Meilisearch and in cargo-rail; fixing 'undeclard features detection' to avoid 'borrowed features' sneaking through. added 'auto-fix' undeclared features w/ warning for manual. This is a big win. ([cf1f5ee](https://github.com/loadingalias/cargo-rail/commit/cf1f5ee42b4d4b82db815ad8fda7796789988cd0))
- cargo-rail: removing 'cargo-udeps' entirely. ([1a73394](https://github.com/loadingalias/cargo-rail/commit/1a73394b02b9ff85bad73570b137c9965b52eb14))



## [0.4.2](https://github.com/loadingalias/cargo-rail/compare/v0.4.1...v0.4.2) - 2025-12-11

### 📦 Other Changes

- cargo-rail: chore: .gitignore ([327ffec](https://github.com/loadingalias/cargo-rail/commit/327ffec58e6032cb6f1c2fb664e9e6bc67c9700d))
- cargo-rail: fixing the lockfile issue in releases ([a02172e](https://github.com/loadingalias/cargo-rail/commit/a02172e8272f707de00b01c3c4f9f000c4984ce0))



## [0.4.1](https://github.com/loadingalias/cargo-rail/compare/v0.4.0...v0.4.1) - 2025-12-11

### 📦 Other Changes

- cargo-rail: push lockfile (this is annoying and needs to be addressed); fixing the 'target' fuzzy matching w/ word boundaries - this prevents the false positives seen in Quiche testing ([bfac137](https://github.com/loadingalias/cargo-rail/commit/bfac137e546003822e639f49b8d89ffe17979996))
- cargo-rail: push lockfile (this is annoying and needs to be addressed); fixing the 'target' fuzzy matching w/ word boundaries - this prevents the false positives seen in Quiche testing ([18497fa](https://github.com/loadingalias/cargo-rail/commit/18497fab10ffa69967525a9e2bc01660f8d6e567))



## [0.4.0](https://github.com/loadingalias/cargo-rail/compare/v0.3.0...v0.4.0) - 2025-12-11

### 📦 Other Changes

- cargo-rail: feat(config): add `cargo rail config sync` for config upgrades; add a new command that safely updates rail.toml when upgrading cargo-rail versions. ([e5cef6b](https://github.com/loadingalias/cargo-rail/commit/e5cef6bcef3444ddfd75dc5c7bee2c077fd235f6))



## [0.3.0](https://github.com/loadingalias/cargo-rail/compare/v0.2.2...v0.3.0) - 2025-12-11

### 🔧 Chores

- update Cargo.lock ([e4b0f4a](https://github.com/loadingalias/cargo-rail/commit/e4b0f4a65d5ae19a664b87837ec5faf22b4e6a84))

### 📦 Other Changes

- crgo-rail: feat: added 'exclude' to the features pruning; updated cfg in rail.toml; upgraded the 'msrv' detection, resolution w/ cfg in rail.toml && added strict testing; fixed the 'quiche' issue where target-triples aren't defined in any TOML, but instead are in '.github/workflows' in yaml/yml files - validated against CF's quiche repo. add cargo-args output format ([5a7337a](https://github.com/loadingalias/cargo-rail/commit/5a7337a7e1529df4b299033ddbfd788b123c9333))



## [0.2.2](https://github.com/loadingalias/cargo-rail/compare/v0.2.0...v0.2.2) - 2025-12-10

### 🔧 Chores

- release v0.2.1 ([e54bd94](https://github.com/loadingalias/cargo-rail/commit/e54bd946c2fe8e8e6b882e833287735ab93b0ca1))

### 📦 Other Changes

- cargo-rail: fixing the codex-rs bug for nested workspaces under change detection; updating the readme/docs/etc. ([7374e03](https://github.com/loadingalias/cargo-rail/commit/7374e03f0daf37fd0096aaea56508741d2478a3b))
- cargo-rail: updating public facing docs, readme, etc. and fully wiring change-detection (dogfooding) ([08ec44a](https://github.com/loadingalias/cargo-rail/commit/08ec44a3e64e9a6036cd621c81b9572bfcf9daf1))
- cargo-rail: README update ([9130377](https://github.com/loadingalias/cargo-rail/commit/91303772af20d2eb2bdaa9edbeb822da8fe66e18))
- cargo-rail: added the 'migrate-hakari.md' file ([e17df42](https://github.com/loadingalias/cargo-rail/commit/e17df42df4cfe72a942909f5761f8bb98546c6ac))
- cargo-rail: update gitignore ([3fbac39](https://github.com/loadingalias/cargo-rail/commit/3fbac397828a11609d1e3811ecae0ccec50a4fb0))
- cargo-rail: chore: update Cargo.lock for v0.2.0 ([f38bf13](https://github.com/loadingalias/cargo-rail/commit/f38bf13785909427d37d10a91a86171ee3015bb2))



## [0.2.0](https://github.com/loadingalias/cargo-rail/compare/v0.1.0...v0.2.0) - 2025-12-05

### 📦 Other Changes

- cargo-rail: feat(unify): add sync subcommand to re-detect and sync targets ([485c65c](https://github.com/loadingalias/cargo-rail/commit/485c65c26a502cd3f67515b69c9f01d460637d3d))



## [0.1.0](https://github.com/loadingalias/cargo-rail/releases/tag/v0.1.0) - 2025-12-05

### ✨ Features

- Update hello message (monorepo) ([5337ee1](https://github.com/loadingalias/cargo-rail/commit/5337ee1d94d2f3cf7d881c73e8ab3c88e06347cf))
- Update feature_a1 in monorepo ([074acae](https://github.com/loadingalias/cargo-rail/commit/074acaeb018a5c6a729035f9db180430cbad5230))
- split repo change ([173a7f0](https://github.com/loadingalias/cargo-rail/commit/173a7f085a98fbbe4212703c0497edd332fb79c4))
- **split**: add interactive confirmation for dry-run mode ([3717d4a](https://github.com/loadingalias/cargo-rail/commit/3717d4a726d141f85e1522fd698474fc0340bbe0))
- **test-crate-a**: add feature_a1 function ([4d1062c](https://github.com/loadingalias/cargo-rail/commit/4d1062c71e7be69d1b33e05b4911fece080cad80))
- **test-crate-b**: add feature_b1 function ([65e0a02](https://github.com/loadingalias/cargo-rail/commit/65e0a0232e2d890434482c887043851ef62acc71))
- add remote function ([bb67c01](https://github.com/loadingalias/cargo-rail/commit/bb67c01c253dab807b5866321f358885eefe9066))
- add another feature from mono ([8412f94](https://github.com/loadingalias/cargo-rail/commit/8412f9477505430f100d372740b75cdca172592e))

### 🐛 Bug Fixes

- Resolve Windows test failures - local path detection and Git path separators ([e2392d6](https://github.com/loadingalias/cargo-rail/commit/e2392d6343b1d4556cd6cda3d7e052a186a867e0))
- Change fix_a1 to return true (monorepo) ([1ec1dcb](https://github.com/loadingalias/cargo-rail/commit/1ec1dcb80a897ccf3f7a5295ad59da9317c79511))
- Add split_repo_feature from split repo ([475fff0](https://github.com/loadingalias/cargo-rail/commit/475fff0355509e74431fa27a75be313c1c2c9e37))
- **status**: detect split status via git-notes instead of local directory ([05cc849](https://github.com/loadingalias/cargo-rail/commit/05cc8497c599e65f6f6f8235281c42824086c1d1))
- **test-crate-a**: add fix_a1 function ([2f2265c](https://github.com/loadingalias/cargo-rail/commit/2f2265cd00e49ad6f32213f9e378dd78d2090be6))
- **test-crate-b**: add fix_b1 function ([117bb43](https://github.com/loadingalias/cargo-rail/commit/117bb433267d6f719519cffeb7c05d793811fdc9))

### 🔧 Chores

- **release**: cargo-rail v0.1.0 ([89931fa](https://github.com/loadingalias/cargo-rail/commit/89931fa91baee51b6bb611ad2b52e6e9133c4c39))
- remove testing artifacts ([62f5d55](https://github.com/loadingalias/cargo-rail/commit/62f5d555aff37ffbfedb53c26593bdbcc4ca89bd))

### 📝 Documentation

- Update PRE_V1.md - VCS Abstraction complete ([3358c89](https://github.com/loadingalias/cargo-rail/commit/3358c8995b3214f55d4c1101f7d814b3f4fdc795))

### 📦 Other Changes

- cargo-rail: updating excluding list for release ([081357a](https://github.com/loadingalias/cargo-rail/commit/081357a4e3e520a7f36d717e0efe4b47e0995b76))
- cargo-rail: fixing demo video access via GH CDN ([573b893](https://github.com/loadingalias/cargo-rail/commit/573b893bb5d2a81b3f4404530190ce247f3e3433))
- cargo-rail: fixed 'optional' feature bug during unify (polars bug) and added demo mp4s. added changelog; updated readme ([145d893](https://github.com/loadingalias/cargo-rail/commit/145d893812206ddf942ee69a865c17827c51fbc1))
- cargo-rail: preparing for asciinema demos ([cfeae1b](https://github.com/loadingalias/cargo-rail/commit/cfeae1b05644fb68227352379d7d9eca05de92a5))
- cargo-rail: fixing the dep bump during releases; auto rollback during failures. fixing the bug in 'mixed defaults detected'.  removing all demos/tapes ([2fa40b5](https://github.com/loadingalias/cargo-rail/commit/2fa40b5a12d4c9dce148e33912e7ffd8a16557d8))
- cargo-rail: audited readme, docs, commands, and config; looks decent. added 'release check' improvements. added pre-built binaries to release.yaml ([8bf7d7a](https://github.com/loadingalias/cargo-rail/commit/8bf7d7a06c4965f2681eda03e3e17a5d61c0e439))
- cargo-rail: cleaning cli artifacts post-refactor. ([8a4685f](https://github.com/loadingalias/cargo-rail/commit/8a4685f90c9e040aa8bbe0103ddde9d4d34e7311))
- cargo-rail: cleaning up inconsistencies in the CLI/cfg. Removed the artifacts from the refactor branch; documented 'syncconfig' for a later date... refactored the 'init' code to reduce duplicated logic. ([09a86c2](https://github.com/loadingalias/cargo-rail/commit/09a86c21631734be202175c6600e530766adb099))
- cargo-rail: update the readme; fix the strict_version_compat options; clarify the msrv design in the readme ([2c1158d](https://github.com/loadingalias/cargo-rail/commit/2c1158dba8973b2119676a8f4007824be379b700))
- cargo-rail: fixing conditional compilation feature flag pruning; updating cmd/cfg in readme ([6fd5bf6](https://github.com/loadingalias/cargo-rail/commit/6fd5bf63e6334b7fab3296ea12c796e5a3c55a44))
- cargo-rail: major clean up; micro perf wins; impoving the DX/CLI usage ([670f72b](https://github.com/loadingalias/cargo-rail/commit/670f72b54af1246b4e8e3d34c0e0cb4632eb9bca))
- cargo-rail: improved the demo/example setup; fixed the feature pruning deleting 'optional' features. ([507058a](https://github.com/loadingalias/cargo-rail/commit/507058a7340effbf788158144929ad2c0b01119a))
- cargo-rail: fixing the time/date in the publish workflow ([43e90b5](https://github.com/loadingalias/cargo-rail/commit/43e90b523cb6e19a77ce242abdfb5d0e4fd3c875))
- cargo-rail: testing found two bugs w/ 'prune_dead_features' that needed to be fixed. ([ed0b521](https://github.com/loadingalias/cargo-rail/commit/ed0b5212984fbe9e5edeed69244f56c4f1262135))
- cargo-rail: added the 'remove_deps' to the rail.toml config; prepped the VHS tapes for examples/ repos; added the gifs themselves for each repo. updating the docs & adding justfile command for docs-gen; cleaned up readme.md - GIF examples need reviews ([bbf8f9a](https://github.com/loadingalias/cargo-rail/commit/bbf8f9a461673e864c8b5d539873706fdeafe093))
- cargo-rail: fixing the issue where pinning transitive deps pulls in 'all-feautres' and breaks the graph. ([8197275](https://github.com/loadingalias/cargo-rail/commit/8197275244327fe647b4fde9fc2000514ad8f5f6))
- cargo-rail: polish around the command and config. deleted mdbook for generated docs + better cli '--help' docs. mdbook is bloated shit for this. ([97f07d6](https://github.com/loadingalias/cargo-rail/commit/97f07d661d556f33983436300aaaae71779c1e14))
- cargo-rail: cleaning ([e0fcf77](https://github.com/loadingalias/cargo-rail/commit/e0fcf772427495a3a565e81386b5b9d1f9e7ddcf))
- cargo-rail: cleaning out 'dead_code' or 'unused'. Fixing error messages. General cleanups. ([6eb566a](https://github.com/loadingalias/cargo-rail/commit/6eb566a4149c2c3b2757faa09a562b2df442cfc8))
- cargo-rail: dx/clarity/cli cleanup ([a0b9270](https://github.com/loadingalias/cargo-rail/commit/a0b927033e8f7c9d8dc859b9165776bdb8a23931))
- cargo-rail: fixed some basic perf stuff; added docs/architecture.md ([09155f4](https://github.com/loadingalias/cargo-rail/commit/09155f4b3b367281c91af0aac0f328f3a39492ec))
- cargo-rail: clean up duplicate methods; merge unify tests; move tests into the integration/ instead of orphaned out. ([c8c636a](https://github.com/loadingalias/cargo-rail/commit/c8c636ad9a259c3fad99bb3411cf9046bd688d84))
- cargo-rail: split the manifest_ops into modular files. ([a9e2c81](https://github.com/loadingalias/cargo-rail/commit/a9e2c816ede7e5957bd110918b1f2d6d10d47f5f))
- cargo-rail: refactored monolith config, unify_analyzer, and fixed error reporting for JSON. ([7aaaad1](https://github.com/loadingalias/cargo-rail/commit/7aaaad1ab772b01cf81d67352a4642b51d5353fb))
- cargo-rail: fixing the weird 'transitive' issue in virtual workspaces; fixing the version bump on 'x.y' versioning layouts. ([93904cd](https://github.com/loadingalias/cargo-rail/commit/93904cd00f74c9d0265c6a3d04b2204c95aab70e))
- cargo-rail: fixing the command structure; final cleanup? ([25d181b](https://github.com/loadingalias/cargo-rail/commit/25d181b2367abe153306e846cc6e325e38f3cf48))
- cargo-rail: adding 'unused dep' detection edge case handling; auto_remove functionality. Testing across real repos (vello, etc.) ([f3e7b69](https://github.com/loadingalias/cargo-rail/commit/f3e7b6983cc750472a4164c9c8d1c0b5fee38b68))
- cargo-rail: major cleanup over the config issues. ([82727bc](https://github.com/loadingalias/cargo-rail/commit/82727bc3a1c74787b2119866146a4ddbc181336e))
- cargo-rail: cleaning up outputs, configs, and commands/subcommands/flags. Extensive testing done; nothing manually - yet. ([f12f0c0](https://github.com/loadingalias/cargo-rail/commit/f12f0c0a78f402448db163822c35a1b08b294c3d))
- cargo-rail: fixing the commands and cleaning up for uniformity ([ad4703c](https://github.com/loadingalias/cargo-rail/commit/ad4703cbbf012663e05b7ee1edf4cd14f6826c2f))
- cargo-rail: fixed the rename blocking issues ([56ad568](https://github.com/loadingalias/cargo-rail/commit/56ad568f17f5aca563597ef9346948d3266f4fdb))
- cargo-rail: fixing the last real bug in unify before v1?! ([914dd85](https://github.com/loadingalias/cargo-rail/commit/914dd85aee638f595ac35cd544012dff37dbd6e6))
- cargo-rail: cleaning up old code, dead commands, flags, etc ([65d2cd8](https://github.com/loadingalias/cargo-rail/commit/65d2cd839f0a920afd0909098fa073cb1f1462dd))
- cargo-rail: improved the change detection && prepped for GHA integration ([11e1fdf](https://github.com/loadingalias/cargo-rail/commit/11e1fdf3dc1af659d1ca36190fb97b9e792ec095))
- cargo-rail: fixed the crate name validation, dry-run, and other command/config issues ([558d67e](https://github.com/loadingalias/cargo-rail/commit/558d67e904a3e9d49fb1b5e1188a6190d9b0ed64))
- cargo-rail: added 'unused dep' detection/removal, msrv resolution, and version pinning. Tightened the 'unify' and 'config' story. Improved the 'split vs crate' configs to share them across the commands. ([d29fbb1](https://github.com/loadingalias/cargo-rail/commit/d29fbb1c355691f4081552804343bb553e73590e))
- cargo-rail: a few fixes to the 'unify' command; MSRV feature integrated. Extensive testing ([4275890](https://github.com/loadingalias/cargo-rail/commit/4275890d38d6f7a59694be3a0d46562e4a390e0a))
- cargo-rail: improving the 'unify' process end to end; tested across 8x cloned repos including jj, datafusion, convex-backend, deno, ferros, helixdb, wasmtime, and rustpython. All green. ([16e6dfd](https://github.com/loadingalias/cargo-rail/commit/16e6dfde6457cc741bc373f2f9929cffc2b9005b))
- cargo-rail: rethought, refactored, and greatly improved the 'cargo rail unify' pipe. consolidated TOML formatting/transforms; improved command structure. ([604b821](https://github.com/loadingalias/cargo-rail/commit/604b8211a57cad2ed87a7e346316882b86ccb6b3))
- cargo-rail: cleaning the unify command up significantly ([e73166d](https://github.com/loadingalias/cargo-rail/commit/e73166d3dbb562f36001bbcd20949ce44e374042))
- cargo-rail: fixing the unify w/ minimization. ([97a9bab](https://github.com/loadingalias/cargo-rail/commit/97a9babe055bff23d59f6c480a8c5b59bad34b12))
- cargo-rail: fixing the feaure minimization ([99ccdcb](https://github.com/loadingalias/cargo-rail/commit/99ccdcb036ddf45e9f35a0e2b03d839b0577b69c))
- cargo-rail: fixed nextest issue ([40582e5](https://github.com/loadingalias/cargo-rail/commit/40582e51ef01b0e251e809883ed2dc65c52ebb92))
- cargo-rail: updated the unify command to automatically select the ideal, minimal set of features(in parallel) needed to resolve the unification. ([8911df5](https://github.com/loadingalias/cargo-rail/commit/8911df541ac2e14e7b2277f7c29ae9f77eaa6c88))
- cargo-rail: added proper TOML formatter for the codebase and cleaned out the manual TOML editing/etc. added a proper backup && undo command. ([dc59279](https://github.com/loadingalias/cargo-rail/commit/dc59279ed50c24772f4cdb91227b9b082d478248))
- cargo-rail: added an 'undo' sub-command for the 'unify' command to ensure that reversing unification is easy and low friction. added the ([22e8241](https://github.com/loadingalias/cargo-rail/commit/22e8241bd813600d1a8f71f33f47267c04a01f83))
- cargo-rail: fixing PR branch creation on split repo -> monorepo sync. TOML fmt is deeply broken across the codebase at this point ([0759727](https://github.com/loadingalias/cargo-rail/commit/07597270cf0efcb2c9c618f8a941603e6431f82a))
- cargo-rail: fixing mono-to-mono split. ([47328ad](https://github.com/loadingalias/cargo-rail/commit/47328ada81c47f42969d150b442d5c3de2705163))
- cargo-rail: fixes to nextest integration and watch mode via bacon ([7734ee2](https://github.com/loadingalias/cargo-rail/commit/7734ee27875a9f44c433bde409f56745b7fea2ca))
- cargo-rail: fixing the features/configs in unify... I think we've done it! ([06d01d2](https://github.com/loadingalias/cargo-rail/commit/06d01d2d8122b8fb461e5b4bcdc2fafc5f64ab1e))
- cargo-rail: fixing all-features issues; transitive chains aren't working ([335df3e](https://github.com/loadingalias/cargo-rail/commit/335df3ef4a5c1d2270b4065543b4b13e22902269))
- cargo-rail: updating the 'unify' process and transitive nightmare; cleaning up the split/sync commands and rail.toml configs. ([99220c7](https://github.com/loadingalias/cargo-rail/commit/99220c76296d44a0e95787ba4105c99471d46efd))
- cargo-rail: working on DX; formatting Cargo.toml changes; the rail.toml config. fixing the 'unify' bypassing the cache; fixing the target-triple detection logic. added parallel dep analysis/unification. ([9b98402](https://github.com/loadingalias/cargo-rail/commit/9b984024f9303a4e508f68cd118678de10d2b848))
- cargo-rail: fixing the "-dr' alias for the cargo rail init command. ([9a9e15f](https://github.com/loadingalias/cargo-rail/commit/9a9e15fc23fd613364b573a70fd05ef00785fbe9))
- cargo-rail: auto-detect, populate, and dedup the targets list in rail.toml on init command; cleaning the 'apply' from commands now that we've got clean '-d/-dr/--dry-run' safety commands. adding 'allow-renamed' command to the rail.toml; fixing the unify Cargo adjustments. ([d630a37](https://github.com/loadingalias/cargo-rail/commit/d630a37f9c8778c3a4999f1c6c00a073306e23e7))
- cargo-rail: manual testing fixes - remove 'analyze' and 'plan' commands in favor of a centralized '-d, -dr, --dry-run' instead. ([b53ecc1](https://github.com/loadingalias/cargo-rail/commit/b53ecc15086421bb170289a06d82ffbc4724fab0))
- cargo-rail: docs cleaning; test cleaning ([36c35d2](https://github.com/loadingalias/cargo-rail/commit/36c35d2275496df5272fc23b937d60855d9611f4))
- cargo-rail: cleaning old methods ([22d5a79](https://github.com/loadingalias/cargo-rail/commit/22d5a7980c42a6dbc07f67549ee75cca087367e6))
- cargo-rail: updates to README.md; removing old config sync, security policy, etc. ([78e86c8](https://github.com/loadingalias/cargo-rail/commit/78e86c862b482f025433c6c3ff1eb8bc84f40491))
- cargo-rail: pre v1 commit - it's CLOSE ([ec5cb68](https://github.com/loadingalias/cargo-rail/commit/ec5cb68b806b13c5ae34dcbf62093d1a229eaa15))
- cargo-rail: added metadata cache via FNV-1a instead of adding sha2/hex deps. extensive testing updates and fixes across the codebase for those end-to-end test bugs. ([6c4325a](https://github.com/loadingalias/cargo-rail/commit/6c4325ad49e54bdb5b93941d25cb85cdadaa3e35))
- cargo-rail: working on the changelogs; testing; DX/UX for v1 ([23cb6d3](https://github.com/loadingalias/cargo-rail/commit/23cb6d36acbd133a25e0f0ab0f3e549228014536))
- cargo-rail: added the changelog updates/improvements. ([12a33c1](https://github.com/loadingalias/cargo-rail/commit/12a33c1da2d4aac91e6ef6e25c5ef477a7a5ab96))
- cargo-rail: update readme ([599bd3d](https://github.com/loadingalias/cargo-rail/commit/599bd3dd632337249cde51424e18cf760301a2a1))
- cargo-rail: added release pipe + changelog ([f3aaa1f](https://github.com/loadingalias/cargo-rail/commit/f3aaa1fd2c10d0604e94c1a6e7589837d128bb8e))
- cargo-rail: fmt ([e231702](https://github.com/loadingalias/cargo-rail/commit/e2317028fde0b64ed78957305455c10fd4ce523f))
- cargo-rail: better 'init' testing; end to end testing; improved error messages. fixed 'audit/deny' in justfile ([11f31d0](https://github.com/loadingalias/cargo-rail/commit/11f31d001d86b651376f5f81308096be2a587b6e))
- cargo-rail: fixed the init; rail.toml template; testing; added README.md. ([001c265](https://github.com/loadingalias/cargo-rail/commit/001c265822405aebf9735543dbaf1bc1f6eaf4a4))
- cargo-rail: updating/streamlining CLI; fixing the panic for unwrap/errors. ([696bfc1](https://github.com/loadingalias/cargo-rail/commit/696bfc1aad27b90c72a7a11723b11cb44b03a1ae))
- cargo-rail: fixing the Windows CI issue - again. ([89d5e2c](https://github.com/loadingalias/cargo-rail/commit/89d5e2cedb9a8fc35bf2716760b850a98e4ceb24))
- cargo-rail: fixed CI issue; added testing and gitnotes robustness. cleaned some todo items up ([7cc7ac1](https://github.com/loadingalias/cargo-rail/commit/7cc7ac1a6aecb9d3bff2bdc40d95b5ee51f5145f))
- cargo-rail: fixing the Windows path issues ([72d5236](https://github.com/loadingalias/cargo-rail/commit/72d52361c12e1be1720bf20043d31dcbe21584c8))
- cargo-rail: fixing windows fs issues ([527eebc](https://github.com/loadingalias/cargo-rail/commit/527eebc3a030f438ae52ac31647c66c220030b70))
- cargo-rail: major updates - change detection, version smart merging, and so much more ([dcbadb9](https://github.com/loadingalias/cargo-rail/commit/dcbadb9535688f1b4c7c8221dfdbda10899f3189))
- cargo-rail: fmt ([5fbdbbf](https://github.com/loadingalias/cargo-rail/commit/5fbdbbf38ecdf14c42c4a6b7517ccf6acd9c9c5f))
- cargo-rail: readme ([14bdf1f](https://github.com/loadingalias/cargo-rail/commit/14bdf1f7dfc2e9d7e0319e39873f4a4595cf5af0))
- cargo-rail: more cleaning; new change detection for split/sync/test runners; added test infrastructure to catch weird cross platform issues early ([4c3970a](https://github.com/loadingalias/cargo-rail/commit/4c3970a22113959cf71dd7c386d3b1d29a082c63))
- cargo-rail: finally, no more Hakari! ([03632d2](https://github.com/loadingalias/cargo-rail/commit/03632d2e2d74ad8b42d85d764a3d8d3636cf8cf2))
- cargo-rail: replaced cargo-hakari and workspace hack crates forever. ([f96f71f](https://github.com/loadingalias/cargo-rail/commit/f96f71f00e4447b872255f3a754d14be67cf1f14))
- csrgo-rail: reorganized; better cargo metadata usage; git/jj compatibility; cleaner repo organization - better testing. ([485df1f](https://github.com/loadingalias/cargo-rail/commit/485df1f8f5c9b2ff1145ccf8b3304c07bf8aa540))
- cargo-rail: bump clap ([05009e9](https://github.com/loadingalias/cargo-rail/commit/05009e92802b646b49267873e75248e59bdc60f2))
- cargo-rail: cleaning it up ([19594cf](https://github.com/loadingalias/cargo-rail/commit/19594cf6df69c50586fd709d5775f10ccc036519))
- cargo-rail: refactoring, slimming down, and balancing the architecture on a strong graph ([9af5154](https://github.com/loadingalias/cargo-rail/commit/9af515442ff568e992425628e60f8efa9d9e1acc))
- cargo-rail: updating the workspace graph to include 'visibility'; added the 'quality' engine. ([030d3f2](https://github.com/loadingalias/cargo-rail/commit/030d3f204ad099827f1fad74221694c3f1c2ef22))
- cargo-rail: fixed the WorkspaceContext not being passed efficiently; all commands now use the single main.rs WorkspaceContext; added rustdocs ([f48c97e](https://github.com/loadingalias/cargo-rail/commit/f48c97ea2a336b56d2243c96deb772275a7672eb))
- cargo-rail: cleaning scattered 'TODOs' ([52cd5e5](https://github.com/loadingalias/cargo-rail/commit/52cd5e511a264f543805f27105cb70ad1066acdd))
- cargo-rail: fixing .gitignore ([63b2a02](https://github.com/loadingalias/cargo-rail/commit/63b2a0271af356509e570184098415a9d767204f))
- cargo-rail: added changelog via winnow instead of 'git-cliff-core' and refactored split/sync for the ExecutionEngine. working on release workflow ([c489c01](https://github.com/loadingalias/cargo-rail/commit/c489c01a614be4c2ba61f8fcb9ce7cc32154bb70))
- cargo-rail: fixing the junit paths/config; fixing the Windows path issues still. ([69c2bdd](https://github.com/loadingalias/cargo-rail/commit/69c2bdd772948f65b04cfe56b829f4067a7c6062))
- carrgo-rail: wired JUnit reporting in CI; added a proper utils.rs for paths/urls/etc. across platforms ([6362008](https://github.com/loadingalias/cargo-rail/commit/6362008166389a474931bf0ce22d2a4e4ccf0591))
- cargo-rail: simple updates ([9160538](https://github.com/loadingalias/cargo-rail/commit/9160538127ec31c805dad77d4163f440ab54ee11))
- cargo-rail: a tolerable cleanup to the readme.md; started working throught the performance fixes: batching, parallelism, efficiencies/caches. ([975b743](https://github.com/loadingalias/cargo-rail/commit/975b743117b8a0122b031e7075d48eb1c7a52d45))
- cargo-rail: added the third pillar (policies/lints/manifest) and the beginning of the publishing/release plan/pipe for Rust ([c5375f0](https://github.com/loadingalias/cargo-rail/commit/c5375f00215982788ccc38a6239e50b1906a53da))
- cargo-rail: updating the deny.toml now that we've removed the heaviness of Gix/Git-Cliff/etc. ([35c3b7e](https://github.com/loadingalias/cargo-rail/commit/35c3b7e4e8f1cdcc78b7ceb98ac6b92a76f172e7))
- cargo-rail: added petgraph && build WorkspaceGraph; implemented the algos needed and wired it up. Full '--git' integration. ([0d58870](https://github.com/loadingalias/cargo-rail/commit/0d588702f4eee8d1840e3179bed92014092adeec))
- cargo-rail: clean up && audit ([5da2fb0](https://github.com/loadingalias/cargo-rail/commit/5da2fb0cbc841e1806c0e8a79e76c055847c11e6))
- cargo-rail: swapped gix for system git ops; it's much cleaner, lighter, and no perf was lost ([4c1123e](https://github.com/loadingalias/cargo-rail/commit/4c1123e9a1d8069eba05bfcbf9ffef7a3a0f85b9))
- cargo-rail: test crate ([7afc061](https://github.com/loadingalias/cargo-rail/commit/7afc0618794950435c6ed57e4d5b9a26f20f05c9))
- cargo-rail: manual testing and updates to UX/ergonomics. ([9fc2112](https://github.com/loadingalias/cargo-rail/commit/9fc2112747a7f16036039754a558662bb1d1fb7a))
- cargo-rail: fixing CI matrix and Windows issues ([fba854b](https://github.com/loadingalias/cargo-rail/commit/fba854bf98f3f30c0dd775171d3d755c9159fc9b))
- cargo-rail: expanding CI/CD coverage; fixing platform specific issues ([72ff609](https://github.com/loadingalias/cargo-rail/commit/72ff6098dce6d636c2fde394d2a561518d4f3333))
- cargo-rail: adding finalize && publish logic; testing the release workflow ([aa79590](https://github.com/loadingalias/cargo-rail/commit/aa79590105cc8c30555429b58f0ac4479b10298e))
- cargo-rail: fixing the failing arm64 test w/ better test setup in mapping.rs ([e70d129](https://github.com/loadingalias/cargo-rail/commit/e70d129053c9fe41c4b7fa62de8e5d6ef56f4a71))
- cargo-rail: fixing the git identity issue (real bug fix). ([e22e046](https://github.com/loadingalias/cargo-rail/commit/e22e0468017c4c983b561ad51b9ae53d2cabd49e))
- cargo-rail: formatting; deny.toml additions because, alas, git-cliff is a shitshow. ([35b67b9](https://github.com/loadingalias/cargo-rail/commit/35b67b9de3f9347080964070acf3eb30daae6da4))
- cargo-rail: housekeeping/docs/fmt; prepare command finalization ([d93c34a](https://github.com/loadingalias/cargo-rail/commit/d93c34a7a1b12c38085f997a0299e3b7f63962c4))
- cargo-rail: added 'git-cliff-core' for changelogs; added release plan scaffolding and tightened errors/parallelism/progress execution/UI. ([45d6a77](https://github.com/loadingalias/cargo-rail/commit/45d6a77d2800d1b3998177ac36c5a9e0e5bcf942))
- cargo-rail: cleaning it up; improving the README.md; preparing for next steps ([8c88acb](https://github.com/loadingalias/cargo-rail/commit/8c88acbe1141e588e911a41bab325dcbedfb7e4e))
- cargo-rail: cleaned it all up. this needs to be a rust-first, rust-only tool for monorepos. I don't know what I was thinking w/ polyglot nonsense. It's all shit next to rust. wired parallel processing up via rayon and added parallel progress via linya. ([453e248](https://github.com/loadingalias/cargo-rail/commit/453e2486c2864a94bb8debfdde7b597a65706a29))
- cargo-rail: cleaned dead dependencies and fixed some dead code leftovers. properly integrated the RailError system; updated the Linya integration for parallel progress bars (we'll need rayon later). added JS/TS NodeAdapter. ([8cde7d7](https://github.com/loadingalias/cargo-rail/commit/8cde7d7d1ccb1c4bf1ad0302e4df79d0d76b6380))
- cargo-rail: fixed deny.toml 'ignore' list ([fe5c1f8](https://github.com/loadingalias/cargo-rail/commit/fe5c1f8872a4f0dd0974c131b23f21c1a490f84d))
- cargo-rail: added UX features; cleaned up and integrated correctly. cleaned up warnings and errors ([7623b4e](https://github.com/loadingalias/cargo-rail/commit/7623b4e360c8c5a401d299344f90e21d353f7be8))
- cargo-rail: improving the architecture for polyglot features. integrated the new architecture and cleaned the original up. added 'rail doctor' command. ([979f660](https://github.com/loadingalias/cargo-rail/commit/979f66057718b48526656c3e46db64ebbea584e6))
- cargo-rail: fixed testing; added 'dry-run' default option && 'apply' flag. some UX/ergonomics cleanups. update README.md ([e5416b2](https://github.com/loadingalias/cargo-rail/commit/e5416b2d04f0560c5ccd2a7b3d98f2a3ba30e962))
- cargo-rail: add git-notes conflict resolution guide. various testing changes. ([a007cdb](https://github.com/loadingalias/cargo-rail/commit/a007cdbb89cf72f46b71369f6e620c36f76ba7d7))
- cargo-rail: fix the failing tests and parallel testing helper issue. ([eedd113](https://github.com/loadingalias/cargo-rail/commit/eedd113b919e1b4128b1a2b227c54171ff7d7c52))
- cargo-rail: implemented proper security model; protected monorepo. updated README.md ([49498c5](https://github.com/loadingalias/cargo-rail/commit/49498c5013e9432ca7bf0682ec774abc9a8678ef))
- cargo-rail: split out the LanguageAdapter for a cleaner workflow later. fixed the test failures and added integration tests. added IndexMap to avoid ts collisions and fixed branch reference handling. ([f3f6af1](https://github.com/loadingalias/cargo-rail/commit/f3f6af1ad63267677af219e8963d88b2811a12a2))
- cargo-rail: split is working; history preserved. sync is bi-directional and working; git notes work. updates are smart; instant. ([b9cc8a0](https://github.com/loadingalias/cargo-rail/commit/b9cc8a0d254f4953573259c40ed3f15fa46e93b7))
- cargo-rail: testing the bi-directional sync ([5d8d651](https://github.com/loadingalias/cargo-rail/commit/5d8d6518e28fe16cc0d6642cffbe028dc7d9e7f6))
- cargo-rail: first real commit; we're getting close. sync and remote sync; bidirectional is close. testing ([9ae7dd3](https://github.com/loadingalias/cargo-rail/commit/9ae7dd37cdbb724aeb49e4ba989723e7dd67654c))
- cargo-rail: scaffold the crate ([fc7b3b6](https://github.com/loadingalias/cargo-rail/commit/fc7b3b6d0feb467fda2dda7e121bcb0515b87b6e))
- Initial commit ([7b2f01e](https://github.com/loadingalias/cargo-rail/commit/7b2f01ebe8bfc8f460749ed79d59bc874da58d98))

### ⚡ Performance

- Add WorkspaceContext and cache HEAD commits in sync loops ([f649f90](https://github.com/loadingalias/cargo-rail/commit/f649f903e4359d049e7d612dda4037b348b28809))

### ♻️ Refactoring

- **release**: enhance code quality and complete prepare command ([32e2f5d](https://github.com/loadingalias/cargo-rail/commit/32e2f5da3847c8db207743cf155d74329ab11d6b))

### ✅ Testing

- changes to both crates ([c7d1c4b](https://github.com/loadingalias/cargo-rail/commit/c7d1c4befc34abeb25d85704f4016d1be8c842b9))
- unrelated change ([6ebadda](https://github.com/loadingalias/cargo-rail/commit/6ebaddadabe2b09674732983c3e330fecdb6ae5c))
- changes to both crates ([e9818f0](https://github.com/loadingalias/cargo-rail/commit/e9818f02536c73b8311f9a927478555a88da0d08))
- mono change 2 ([ea027d3](https://github.com/loadingalias/cargo-rail/commit/ea027d3a8959c0bedd768661ed198d543beedddc))
- add comment to test-crate-a ([9dedfe5](https://github.com/loadingalias/cargo-rail/commit/9dedfe53344542d5a0e1577ee5c4158789d89a9e))
- add test from remote repo ([2583f68](https://github.com/loadingalias/cargo-rail/commit/2583f689060cf31f7898033e10f6f3573aeb58c7))

