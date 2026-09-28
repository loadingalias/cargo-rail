//! Observe completed compiler modules without changing backend behavior.

use std::any::Any;
use std::sync::{Arc, Mutex};

use rustc_codegen_ssa::{CompiledModules, CrateInfo, TargetConfig, traits::CodegenBackend};
use rustc_metadata::{EncodedMetadata, creader::MetadataLoaderDyn};
use rustc_middle::{dep_graph::WorkProductMap, ty::TyCtxt, util::Providers};
#[cfg(rail_incremental_session)]
use rustc_session::IncrCompSession;
#[cfg(rail_early_session_backend)]
use rustc_session::{CodegenBackendInit, EarlySession};
use rustc_session::{
    Session,
    config::{OutputFilenames, PrintRequest},
};
#[cfg(not(rail_early_session_backend))]
use rustc_span::Symbol;

use crate::CrateType;
use crate::native_input_protocol::NativeCodegenObservation;

pub(crate) fn configure(
    config: &mut rustc_interface::interface::Config,
    observation: Arc<Mutex<NativeCodegenObservation>>,
) {
    config.make_codegen_backend = Some(Box::new(move |sess| {
        let diagnostics = rustc_session::EarlyDiagCtxt::new(sess.opts.error_format);
        let inner = rustc_interface::util::get_codegen_backend(
            &diagnostics,
            &sess.opts.sysroot,
            sess.opts.unstable_opts.codegen_backend.as_deref(),
            &sess.target,
        );
        Box::new(ObservedBackend { inner, observation })
    }));
}

struct ObservedBackend {
    inner: Box<dyn CodegenBackend>,
    observation: Arc<Mutex<NativeCodegenObservation>>,
}

impl CodegenBackend for ObservedBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    #[cfg(not(rail_early_session_backend))]
    fn init(&self, sess: &Session) {
        self.inner.init(sess)
    }

    #[cfg(rail_early_session_backend)]
    fn init(&mut self, sess: &EarlySession) -> CodegenBackendInit {
        self.inner.init(sess)
    }

    fn print(&self, req: &PrintRequest, out: &mut String, sess: &Session) {
        self.inner.print(req, out, sess)
    }

    #[cfg(not(rail_early_session_backend))]
    fn target_config(&self, sess: &Session) -> TargetConfig {
        self.inner.target_config(sess)
    }

    #[cfg(rail_early_session_backend)]
    fn target_config(&self, sess: &EarlySession) -> TargetConfig {
        self.inner.target_config(sess)
    }

    fn supported_crate_types(&self, sess: &Session) -> Vec<CrateType> {
        self.inner.supported_crate_types(sess)
    }

    fn print_passes(&self) {
        self.inner.print_passes()
    }

    fn print_version(&self) {
        self.inner.print_version()
    }

    #[cfg(not(rail_early_session_backend))]
    fn replaced_intrinsics(&self) -> Vec<Symbol> {
        self.inner.replaced_intrinsics()
    }

    #[cfg(not(rail_early_session_backend))]
    fn fallback_intrinsics(&self) -> Vec<Symbol> {
        self.inner.fallback_intrinsics()
    }

    #[cfg(not(rail_early_session_backend))]
    fn thin_lto_supported(&self) -> bool {
        self.inner.thin_lto_supported()
    }

    fn has_zstd(&self) -> bool {
        self.inner.has_zstd()
    }

    fn has_mnemonic(&self, sess: &Session, mnemonic: &str) -> bool {
        self.inner.has_mnemonic(sess, mnemonic)
    }

    fn metadata_loader(&self) -> Box<MetadataLoaderDyn> {
        self.inner.metadata_loader()
    }

    fn provide(&self, providers: &mut Providers) {
        self.inner.provide(providers)
    }

    fn target_cpu(&self, sess: &Session) -> String {
        self.inner.target_cpu(sess)
    }

    fn codegen_crate<'tcx>(&self, tcx: TyCtxt<'tcx>) -> Box<dyn Any> {
        self.inner.codegen_crate(tcx)
    }

    fn print_pass_timings(&self) {
        self.inner.print_pass_timings()
    }

    fn print_statistics(&self) {
        self.inner.print_statistics()
    }

    fn print_statistics_json(&self) -> String {
        self.inner.print_statistics_json()
    }

    fn link(
        &self,
        sess: &Session,
        modules: CompiledModules,
        info: CrateInfo,
        metadata: EncodedMetadata,
        outputs: &OutputFilenames,
    ) {
        self.inner.link(sess, modules, info, metadata, outputs)
    }

    fn join_codegen(
        &self,
        ongoing: Box<dyn Any>,
        sess: &Session,
        #[cfg(rail_incremental_session)] incremental: Option<&IncrCompSession>,
        outputs: &OutputFilenames,
        info: &CrateInfo,
    ) -> (CompiledModules, WorkProductMap) {
        #[cfg(not(rail_incremental_session))]
        let (modules, products) = self.inner.join_codegen(ongoing, sess, outputs, info);
        #[cfg(rail_incremental_session)]
        let (modules, products) = self.inner.join_codegen(ongoing, sess, incremental, outputs, info);
        let observed = match self.inner.name() {
            "llvm" => NativeCodegenObservation::Llvm,
            "cranelift" => NativeCodegenObservation::Cranelift {
                separate_assembly: modules
                    .modules
                    .iter()
                    .chain(&modules.allocator_module)
                    .any(|module| module.global_asm_object.is_some()),
            },
            _ => NativeCodegenObservation::Unsupported,
        };
        if let Ok(mut observation) = self.observation.lock() {
            *observation = observed;
        }
        (modules, products)
    }
}
