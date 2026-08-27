//! Front-door integration coverage for cargo-rail commands and workspace contracts.

mod helpers;
mod test_cache_installation;
mod test_check;
mod test_clean;
mod test_compiler_observation;
mod test_config;
mod test_distributed_compilation;
mod test_error_handling;
mod test_frontdoor_smoke;
mod test_git_notes;
mod test_init;
mod test_instrumentation;
mod test_msrv;
mod test_native_cache_fixture;
mod test_nested_workspace;
mod test_output_contracts;
mod test_ownership_index;
mod test_plan;
mod test_plan_apply;
mod test_plan_consumers;
mod test_release_changelog;
mod test_resolution_view;
#[cfg(unix)]
mod test_scale_fixture;
mod test_source_snapshot;
mod test_split;
mod test_surface;
mod test_sync;
mod test_undeclared_features;
mod test_unify;
mod test_unify_undo;
mod test_unused_detection;
mod test_workspace_snapshot;
