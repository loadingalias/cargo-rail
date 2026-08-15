---
"cargo-rail" = "major"
---

Established one deterministic compiler-fact analysis scheduler over captured manifest and source inputs. Stable
diagnostic collection now consumes scheduler-owned Cargo views and fixed arguments, and fact-required workspace
compilations bypass ordinary compiler-result reuse unless a later combined protocol proves the required sidecar.
Incomplete fact runs are no longer reusable. Removed the superseded public `AnalysisConfiguration` representation and
the public collector constructor that could recapture workspace state independently. Added the bounded canonical
typed-fact fragment protocol, including exact run/compiler/driver/unit authority, root-independent source identities,
byte-bounded definition and visibility spans, entry points, typed edges, conservative roots, and explicit completeness
coverage. Typed and diagnostic requirements now collapse only identical Cargo checks while compile-only doctests remain
separate. Authenticated release drivers are exact-toolchain sibling components with guarded execution paths on Linux
and Windows; source installations perform no driver discovery. Fact sidecars are admitted only through canonical
content-addressed compiler-message announcements and are revalidated before use. Exact run-independent fact objects
and complete view manifests now reuse the shared local CAS across moved workspace roots; missing, corrupt, partial, or
authority-mismatched sets remain misses. A clean-root production-collector workload proves one combined cold schedule
uses half the Cargo views of independent diagnostics and typed collectors, then reopens identical facts with zero Cargo
views after removing the driver executable. Native compatibility jobs compile and execute the matched corpus on every
supported Linux and Windows host, and release archive smoke tests load the bundled driver against its exact
authenticated rustc runtime.
