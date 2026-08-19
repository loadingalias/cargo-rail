#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
measure="$repo_root/scripts/bench/measure-command.py"

usage() {
  echo "usage: $0 <prepare|seal-identity|build|resources|worker-start|worker-stop|sccache-scheduler-start|" \
    "sccache-worker-start|sccache-client-prepare|sccache-stop|reset-client|reset-measure|run|measure|report>" \
    "<run-id> [arguments...]" >&2
  echo "       measure <run-id> <1-3 rounds> <endpoint> <capability-id>" >&2
  echo "       three rounds are an operator-bounded qualification; p95 is the maximum observed sample" >&2
  exit 2
}

phase="${1:-}"
run_id="${2:-}"
[[ "$run_id" =~ ^task10-[A-Za-z0-9][A-Za-z0-9._-]*$ && "$run_id" != *..* ]] || usage
state="$repo_root/benchmark_results/distributed-execution-state/$run_id"
results="$repo_root/benchmark_results/distributed-execution/$run_id"
identity="$state/identity"
binary="$repo_root/target/release/cargo-rail"
worker="$repo_root/target/release/cargo-rail-distributed-worker"
measure_results="$results/measure"
measure_state="$state/measure"
measure_endpoint=""
measure_capability_id=""
measure_endpoint_network=""
measure_rounds=0
sccache_bin=""
sccache_version=""
sccache_dist_client_config="$state/sccache-dist-client.conf"
active_sccache_directory=""

release_measurement() {
  [[ -z "$active_sccache_directory" ]] || measure_stop_sccache "$active_sccache_directory"
  [[ ! -d "$measure_lock" ]] || [[ "$(cat "$measure_lock/pid" 2>/dev/null || true)" != "$$" ]] || rm -rf -- "$measure_lock"
}

measure_lock="$repo_root/target/benchmarks/.distributed-execution-measure.lock"

require_private_file() {
  local path="$1" mode
  [[ -f "$path" && ! -L "$path" ]] || {
    echo "distributed qualification identity is not a real file: $path" >&2
    exit 2
  }
  if mode="$(stat -f '%Lp' "$path" 2>/dev/null)"; then
    :
  else
    mode="$(stat -c '%a' "$path")"
  fi
  [[ "$mode" == 600 ]] || {
    echo "distributed qualification identity is not private: $path ($mode)" >&2
    exit 2
  }
}

build_binaries() {
  cargo build --manifest-path "$repo_root/Cargo.toml" --package cargo-rail --bins --all-features --release --locked
  [[ -x "$binary" && -x "$worker" ]] || {
    echo "distributed qualification binaries are unavailable" >&2
    exit 2
  }
}

build() {
  [[ "$#" -eq 0 ]] || usage
  build_binaries
}

qualify_resources() {
  [[ "$#" -eq 0 ]] || usage
  [[ -d "$state" && -d "$results" ]] || {
    echo "distributed resource qualification is not prepared: $run_id" >&2
    exit 2
  }
  for tool in sudo systemd-run; do
    command -v "$tool" >/dev/null || {
      echo "distributed resource qualification requires $tool" >&2
      exit 2
    }
  done
  sudo -n true || {
    echo "distributed resource qualification requires non-interactive systemd service authority" >&2
    exit 2
  }
  build_binaries
  local rustc bubblewrap unit_key protocol qualified
  rustc="$(rustup which rustc)"
  bubblewrap="$(command -v bwrap || true)"
  [[ -n "$bubblewrap" && -x "$bubblewrap" ]] || {
    echo "distributed resource qualification requires a native Bubblewrap installation" >&2
    exit 2
  }
  unit_key="$(printf '%s' "$run_id-resources" | sha256sum | cut -c1-16)"
  protocol="$($worker protocol-version)"
  qualified="$(sudo -n systemd-run --quiet --wait --pipe --collect \
    --unit="cargo-rail-distributed-resources-$unit_key" \
    --service-type=exec \
    --property="User=$(id -u)" \
    --property="Group=$(id -g)" \
    --property="Delegate=cpu memory pids" \
    --property="KillMode=control-group" \
    --property="WorkingDirectory=/" \
    "$worker" qualify-bubblewrap "$rustc" "$bubblewrap")"
  [[ "$qualified" == "$protocol" ]] || {
    echo "distributed resource qualification returned an invalid protocol result" >&2
    exit 1
  }
  jq -n \
    --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg protocol_version "$protocol" \
    --arg run_id "$run_id" \
    --arg target "${DEV_MACHINE_TARGET:-unknown}" \
    --arg instance_type "${DEV_MACHINE_INSTANCE_TYPE:-unknown}" \
    --arg host "$(uname -a)" \
    --arg cgroup_controllers "$(cat /sys/fs/cgroup/cgroup.controllers)" \
    --arg bubblewrap "$($bubblewrap --version)" \
    --arg worker_sha256 "$(sha256sum "$worker" | awk '{print $1}')" \
    '{
      schema_version: 1,
      generated_at: $generated_at,
      run_id: $run_id,
      status: "passed",
      protocol_version: ($protocol_version | tonumber),
      dev_machine_target: $target,
      instance_type: $instance_type,
      host: $host,
      cgroup_controllers: ($cgroup_controllers | split(" ")),
      bubblewrap: $bubblewrap,
      worker_sha256: $worker_sha256
    }' >"$results/resource-qualification.json"
  chmod 600 "$results/resource-qualification.json"
  jq . "$results/resource-qualification.json"
}

prepare() {
  [[ "$#" -eq 0 ]] || usage
  [[ ! -e "$state" && ! -e "$results" ]] || {
    echo "distributed qualification state already exists: $run_id" >&2
    exit 2
  }
  mkdir -p "$identity" "$results"
  chmod 700 "$state" "$identity" "$results"
  jq -n \
    --arg run_id "$run_id" \
    --arg host "$(hostname)" \
    --arg target "${DEV_MACHINE_TARGET:-unknown}" \
    --arg instance_type "${DEV_MACHINE_INSTANCE_TYPE:-unknown}" \
    '{schema_version: 1, run_id: $run_id, host: $host, target: $target, instance_type: $instance_type}' \
    >"$results/environment.json"
  chmod 600 "$results/environment.json"
}

seal_identity() {
  [[ "$#" -eq 1 ]] || usage
  local role="$1" file
  local -a files=()
  case "$role" in
    worker) files=(authority.pem server.pem server.key) ;;
    client) files=(authority.pem client.pem client.key) ;;
    sccache-scheduler) files=(sccache-client-token sccache-server-token) ;;
    sccache-worker) files=(sccache-server-token) ;;
    sccache-client) files=(sccache-client-token) ;;
    *) usage ;;
  esac
  for file in "${files[@]}"; do
    [[ -f "$identity/$file" && ! -L "$identity/$file" ]] || {
      echo "distributed qualification identity upload is incomplete: $file" >&2
      exit 2
    }
    chmod 600 "$identity/$file"
    require_private_file "$identity/$file"
  done
}

sccache_dist_binary() {
  local binary expected

  binary="$(command -v sccache-dist || true)"
  [[ -n "$binary" && -x "$binary" ]] || {
    echo "distributed qualification requires sccache-dist" >&2
    exit 2
  }
  # Both official binaries use the package name in their clap version string.
  expected="sccache $(sccache_pinned_version)"
  [[ "$($binary --version)" == "$expected" ]] || {
    echo "distributed qualification requires $expected" >&2
    exit 2
  }
  printf '%s\n' "$binary"
}

sccache_dist_token() {
  local path="$1" token

  require_private_file "$path"
  token="$(<"$path")"
  [[ "$token" =~ ^[0-9a-f]{64}$ ]] || {
    echo "distributed qualification token is invalid: $path" >&2
    exit 2
  }
  printf '%s\n' "$token"
}

cluster_network_ip() {
  local network="$1" ip

  case "$network" in
    tailscale)
      ip="$(tailscale ip -4 | sed -n '1p')"
      [[ "$ip" =~ ^100\.([0-9]{1,3}\.){2}[0-9]{1,3}$ ]] || {
        echo "distributed qualification has no Tailscale IPv4 address" >&2
        exit 1
      }
      ;;
    private)
      ip="$(ip -4 -json route get 1.1.1.1 | jq -r '.[0].prefsrc // empty')"
      [[ "$ip" =~ ^10\.([0-9]{1,3}\.){2}[0-9]{1,3}$ \
        || "$ip" =~ ^172\.(1[6-9]|2[0-9]|3[01])\.([0-9]{1,3}\.)[0-9]{1,3}$ \
        || "$ip" =~ ^192\.168\.([0-9]{1,3}\.)[0-9]{1,3}$ ]] || {
        echo "distributed qualification has no private IPv4 route source" >&2
        exit 1
      }
      ;;
    *) usage ;;
  esac
  printf '%s\n' "$ip"
}

cluster_endpoint_valid() {
  local endpoint="$1" host port

  host="${endpoint%:*}"
  port="${endpoint##*:}"
  [[ "$port" =~ ^[1-9][0-9]{3,4}$ && "$port" -le 65535 ]] || return 1
  [[ "$host" =~ ^100\.([0-9]{1,3}\.){2}[0-9]{1,3}$ \
    || "$host" =~ ^10\.([0-9]{1,3}\.){2}[0-9]{1,3}$ \
    || "$host" =~ ^172\.(1[6-9]|2[0-9]|3[01])\.([0-9]{1,3}\.)[0-9]{1,3}$ \
    || "$host" =~ ^192\.168\.([0-9]{1,3}\.)[0-9]{1,3}$ ]]
}

worker_start() {
  [[ "$#" -eq 1 || "$#" -eq 2 ]] || usage
  local port="$1" network="${2:-tailscale}" rustc bubblewrap pid startup bind_ip unit unit_key
  [[ "$port" =~ ^[1-9][0-9]{3,4}$ && "$port" -le 65535 ]] || usage
  for file in authority.pem server.pem server.key; do
    require_private_file "$identity/$file"
  done
  [[ ! -e "$state/worker.pid" && ! -e "$state/worker.unit" ]] || {
    echo "distributed qualification worker already has launch authority" >&2
    exit 2
  }
  for tool in ip jq sudo systemctl systemd-run tailscale; do
    command -v "$tool" >/dev/null || {
      echo "distributed qualification requires $tool" >&2
      exit 2
    }
  done
  sudo -n true || {
    echo "distributed qualification requires non-interactive systemd service authority" >&2
    exit 2
  }
  build_binaries
  rustc="$(rustup which rustc)"
  bubblewrap="$(command -v bwrap || true)"
  [[ -n "$bubblewrap" && -x "$bubblewrap" ]] || {
    echo "distributed qualification requires a native Bubblewrap installation" >&2
    exit 2
  }
  unit_key="$(printf '%s' "$run_id" | sha256sum | cut -c1-16)"
  sudo -n systemd-run --quiet --wait --pipe --collect \
    --unit="cargo-rail-distributed-qualify-$unit_key" \
    --service-type=exec \
    --property="User=$(id -u)" \
    --property="Group=$(id -g)" \
    --property="Delegate=cpu memory pids" \
    --property="KillMode=control-group" \
    --property="WorkingDirectory=/" \
    "$worker" qualify-bubblewrap "$rustc" "$bubblewrap" >/dev/null
  bind_ip="$(cluster_network_ip "$network")"
  : >"$results/worker-events.jsonl"
  : >"$results/worker-stderr"
  chmod 600 "$results/worker-events.jsonl" "$results/worker-stderr"
  unit="cargo-rail-distributed-$unit_key.service"
  printf '%s\n' "$unit" >"$state/worker.unit"
  chmod 600 "$state/worker.unit"
  trap 'sudo -n systemctl stop "$unit" >/dev/null 2>&1 || true; rm -f -- "$state/worker.pid" "$state/worker.unit"' EXIT
  sudo -n systemd-run --quiet --collect \
    --unit="$unit" \
    --service-type=exec \
    --property="User=$(id -u)" \
    --property="Group=$(id -g)" \
    --property="Delegate=cpu memory pids" \
    --property="KillMode=mixed" \
    --property="TimeoutStopSec=150s" \
    --property="WorkingDirectory=/" \
    --property="StandardInput=null" \
    --property="StandardOutput=append:$results/worker-events.jsonl" \
    --property="StandardError=append:$results/worker-stderr" \
    "$worker" serve-mtls-bubblewrap \
    "$rustc" "$bubblewrap" "$bind_ip:$port" \
    "$identity/server.pem" "$identity/server.key" "$identity/authority.pem" 2
  pid="$(sudo -n systemctl show --property=MainPID --value "$unit")"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || {
    echo "distributed qualification worker service has no main process" >&2
    exit 1
  }
  printf '%s\n' "$pid" >"$state/worker.pid"
  chmod 600 "$state/worker.pid"
  for _ in $(seq 1 150); do
    if ! kill -0 "$pid" 2>/dev/null; then
      echo "distributed qualification worker exited during startup" >&2
      sed -n '1,20p' "$results/worker-stderr" >&2
      exit 1
    fi
    startup="$(sed -n '1p' "$results/worker-events.jsonl")"
    if jq -e --arg port "$port" \
      '.transport == "mutual_tls_1_3"
        and .protocol_version == 3
        and .isolation == "bubblewrap_linux_v2"
        and .resource_limits == {
          cpu_period_micros: 100000,
          cpu_quota_micros: 100000,
          max_output_bytes: 67108864,
          max_processes: 64,
          max_stream_bytes: 8388608,
          memory_bytes: 2147483648,
          scratch_bytes: 536870912,
          wall_time_ms: 120000
        }
        and (.address | endswith(":" + $port))' \
      <<<"$startup" >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
  done
  startup="$(sed -n '1p' "$results/worker-events.jsonl")"
  jq -e '
    .transport == "mutual_tls_1_3"
    and .protocol_version == 3
    and .isolation == "bubblewrap_linux_v2"
    and (.isolation_identity | startswith("isolation-v2:sha256:"))
    and .resource_limits == {
      cpu_period_micros: 100000,
      cpu_quota_micros: 100000,
      max_output_bytes: 67108864,
      max_processes: 64,
      max_stream_bytes: 8388608,
      memory_bytes: 2147483648,
      scratch_bytes: 536870912,
      wall_time_ms: 120000
    }
  ' <<<"$startup" >/dev/null || {
    echo "distributed qualification worker emitted no valid startup authority" >&2
    exit 1
  }
  jq -c -n \
    --arg endpoint "$bind_ip:$port" \
    --arg network "$network" \
    --arg capability_id "$(jq -r '.capability_id' <<<"$startup")" \
    --arg isolation "$(jq -r '.isolation' <<<"$startup")" \
    --arg isolation_identity "$(jq -r '.isolation_identity' <<<"$startup")" \
    '{
      schema_version: 1,
      endpoint: $endpoint,
      network: $network,
      capability_id: $capability_id,
      isolation: $isolation,
      isolation_identity: $isolation_identity
    }'
  trap - EXIT
}

worker_stop() {
  [[ "$#" -eq 0 ]] || usage
  local pid unit expected actual active
  [[ -f "$state/worker.pid" && ! -L "$state/worker.pid" \
    && -f "$state/worker.unit" && ! -L "$state/worker.unit" ]] || {
    echo "distributed qualification worker has no launch authority" >&2
    exit 2
  }
  pid="$(<"$state/worker.pid")"
  unit="$(<"$state/worker.unit")"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || {
    echo "distributed qualification worker PID is invalid" >&2
    exit 2
  }
  [[ "$unit" =~ ^cargo-rail-distributed-[0-9a-f]{16}\.service$ ]] || {
    echo "distributed qualification worker unit is invalid" >&2
    exit 2
  }
  [[ "$(sudo -n systemctl show --property=MainPID --value "$unit")" == "$pid" ]] || {
    echo "distributed qualification worker unit changed its main process" >&2
    exit 2
  }
  expected="$(readlink -f "$worker")"
  actual="$(readlink -f "/proc/$pid/exe" 2>/dev/null || true)"
  [[ "$actual" == "$expected" ]] || {
    echo "distributed qualification worker PID does not select the expected binary" >&2
    exit 2
  }
  sudo -n systemctl stop "$unit"
  for _ in $(seq 1 100); do
    active="$(sudo -n systemctl show --property=ActiveState --value "$unit" 2>/dev/null || true)"
    [[ "$active" != active && "$active" != activating && "$active" != deactivating ]] && break
    sleep 0.1
  done
  active="$(sudo -n systemctl show --property=ActiveState --value "$unit" 2>/dev/null || true)"
  [[ "$active" != active && "$active" != activating && "$active" != deactivating ]] || {
    echo "distributed qualification worker did not stop" >&2
    exit 1
  }
  kill -0 "$pid" 2>/dev/null && {
    echo "distributed qualification worker process survived unit cleanup" >&2
    exit 1
  }
  jq -s -e '
    length >= 3
      and .[0].event == "worker_ready"
      and .[-1].event == "worker_stopped"
      and .[-1].active_connections == 0
      and ([.[] | select(.event == "worker_draining")] | length) == 1
      and ([.[] | select(.event == "worker_stopped")] | length) == 1
  ' "$results/worker-events.jsonl" >/dev/null || {
    echo "distributed qualification worker did not prove its bounded drain" >&2
    exit 1
  }
  rm -f -- "$state/worker.pid" "$state/worker.unit"
}

sccache_dist_scheduler_start() {
  [[ "$#" -eq 1 || "$#" -eq 2 ]] || usage
  local port="$1" network="${2:-tailscale}" binary ip endpoint client_token server_token directory config unit unit_key
  local pid status
  [[ "$port" =~ ^[1-9][0-9]{3,4}$ && "$port" -le 65535 ]] || usage
  [[ -d "$state" && -d "$results" ]] || {
    echo "sccache distributed scheduler is not prepared: $run_id" >&2
    exit 2
  }
  [[ ! -e "$state/sccache-scheduler.pid" && ! -e "$state/sccache-scheduler.unit" ]] || {
    echo "sccache distributed scheduler already has launch authority" >&2
    exit 2
  }
  for tool in curl ip jq sudo systemctl systemd-run tailscale; do
    command -v "$tool" >/dev/null || {
      echo "sccache distributed scheduler requires $tool" >&2
      exit 2
    }
  done
  sudo -n true || {
    echo "sccache distributed scheduler requires non-interactive systemd service authority" >&2
    exit 2
  }
  binary="$(sccache_dist_binary)"
  ip="$(cluster_network_ip "$network")"
  endpoint="$ip:$port"
  client_token="$(sccache_dist_token "$identity/sccache-client-token")"
  server_token="$(sccache_dist_token "$identity/sccache-server-token")"
  directory="$state/sccache-scheduler"
  config="$directory/scheduler.conf"
  mkdir -p "$directory"
  chmod 700 "$directory"
  printf '%s\n' \
    "public_addr = \"$endpoint\"" \
    '[client_auth]' \
    'type = "token"' \
    "token = \"$client_token\"" \
    '[server_auth]' \
    'type = "token"' \
    "token = \"$server_token\"" >"$config"
  chmod 600 "$config"
  : >"$results/sccache-scheduler-stdout"
  : >"$results/sccache-scheduler-stderr"
  chmod 600 "$results/sccache-scheduler-stdout" "$results/sccache-scheduler-stderr"
  unit_key="$(printf '%s' "$run_id-sccache-scheduler" | sha256sum | cut -c1-16)"
  unit="cargo-rail-sccache-scheduler-$unit_key.service"
  printf '%s\n' "$unit" >"$state/sccache-scheduler.unit"
  chmod 600 "$state/sccache-scheduler.unit"
  trap 'sudo -n systemctl stop "$unit" >/dev/null 2>&1 || true; rm -f -- "$state/sccache-scheduler.pid" "$state/sccache-scheduler.unit"' EXIT
  sudo -n systemd-run --quiet --collect \
    --unit="$unit" \
    --service-type=exec \
    --property="User=$(id -u)" \
    --property="Group=$(id -g)" \
    --property='KillMode=control-group' \
    --property='TimeoutStopSec=10s' \
    --property='WorkingDirectory=/' \
    --property="StandardOutput=append:$results/sccache-scheduler-stdout" \
    --property="StandardError=append:$results/sccache-scheduler-stderr" \
    /usr/bin/env SCCACHE_NO_DAEMON=1 "$binary" scheduler --config "$config"
  pid="$(sudo -n systemctl show --property=MainPID --value "$unit")"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || {
    echo "sccache distributed scheduler has no main process" >&2
    exit 1
  }
  printf '%s\n' "$pid" >"$state/sccache-scheduler.pid"
  chmod 600 "$state/sccache-scheduler.pid"
  for _ in $(seq 1 150); do
    if ! sudo -n kill -0 "$pid" 2>/dev/null; then
      echo "sccache distributed scheduler exited during startup" >&2
      sed -n '1,30p' "$results/sccache-scheduler-stderr" >&2
      exit 1
    fi
    if status="$(curl -fsS --max-time 2 -H 'Accept: application/json' \
      "http://$endpoint/api/v1/scheduler/status" 2>/dev/null)"; then
      jq -e . <<<"$status" >/dev/null && break
    fi
    sleep 0.1
  done
  if [[ -z "${status:-}" ]] || ! jq -e . <<<"$status" >/dev/null; then
    echo "sccache distributed scheduler did not become ready" >&2
    exit 1
  fi
  jq -n --arg endpoint "$endpoint" '{schema_version: 1, endpoint: $endpoint}'
  trap - EXIT
}

sccache_dist_worker_start() {
  [[ "$#" -eq 2 || "$#" -eq 3 ]] || usage
  local port="$1" scheduler_endpoint="$2" network="${3:-tailscale}" binary ip endpoint server_token bubblewrap directory
  local config runtime_directory build_directory
  local unit unit_key pid status
  [[ "$port" =~ ^[1-9][0-9]{3,4}$ && "$port" -le 65535 ]] || usage
  cluster_endpoint_valid "$scheduler_endpoint" || usage
  [[ -d "$state" && -d "$results" ]] || {
    echo "sccache distributed worker is not prepared: $run_id" >&2
    exit 2
  }
  [[ ! -e "$state/sccache-worker.pid" && ! -e "$state/sccache-worker.unit" ]] || {
    echo "sccache distributed worker already has launch authority" >&2
    exit 2
  }
  for tool in curl install ip jq sudo systemctl systemd-run tailscale; do
    command -v "$tool" >/dev/null || {
      echo "sccache distributed worker requires $tool" >&2
      exit 2
    }
  done
  sudo -n true || {
    echo "sccache distributed worker requires non-interactive systemd service authority" >&2
    exit 2
  }
  binary="$(sccache_dist_binary)"
  bubblewrap="$(command -v bwrap || true)"
  [[ -n "$bubblewrap" && -x "$bubblewrap" ]] || {
    echo "sccache distributed worker requires Bubblewrap" >&2
    exit 2
  }
  ip="$(cluster_network_ip "$network")"
  endpoint="$ip:$port"
  server_token="$(sccache_dist_token "$identity/sccache-server-token")"
  directory="$state/sccache-worker"
  config="$directory/server.conf"
  unit_key="$(printf '%s' "$run_id-sccache-worker" | sha256sum | cut -c1-16)"
  runtime_directory="cargo-rail-sccache-worker-$unit_key"
  build_directory="/tmp/build"
  sudo -n install -d -m 700 -o "$(id -u)" -g "$(id -g)" \
    "$directory" "$directory/toolchains"
  printf '%s\n' \
    "cache_dir = \"$directory/toolchains\"" \
    'toolchain_cache_size = 5368709120' \
    "public_addr = \"$endpoint\"" \
    "scheduler_url = \"http://$scheduler_endpoint\"" \
    '[builder]' \
    'type = "overlay"' \
    "build_dir = \"$build_directory\"" \
    "bwrap_path = \"$bubblewrap\"" \
    '[scheduler_auth]' \
    'type = "token"' \
    "token = \"$server_token\"" >"$config"
  chmod 600 "$config"
  : >"$results/sccache-worker-stdout"
  : >"$results/sccache-worker-stderr"
  chmod 600 "$results/sccache-worker-stdout" "$results/sccache-worker-stderr"
  unit="$runtime_directory.service"
  printf '%s\n' "$unit" >"$state/sccache-worker.unit"
  chmod 600 "$state/sccache-worker.unit"
  trap 'sudo -n systemctl stop "$unit" >/dev/null 2>&1 || true; rm -f -- "$state/sccache-worker.pid" "$state/sccache-worker.unit"' EXIT
  sudo -n systemd-run --quiet --collect \
    --unit="$unit" \
    --service-type=exec \
    --property='KillMode=control-group' \
    --property='TimeoutStopSec=10s' \
    --property='WorkingDirectory=/' \
    --property='PrivateTmp=yes' \
    --property="RuntimeDirectory=$runtime_directory" \
    --property='RuntimeDirectoryMode=0700' \
    --property="StandardOutput=append:$results/sccache-worker-stdout" \
    --property="StandardError=append:$results/sccache-worker-stderr" \
    /usr/bin/env SCCACHE_NO_DAEMON=1 SCCACHE_LOG=warn "$binary" server --config "$config"
  pid="$(sudo -n systemctl show --property=MainPID --value "$unit")"
  [[ "$pid" =~ ^[1-9][0-9]*$ ]] || {
    echo "sccache distributed worker has no main process" >&2
    exit 1
  }
  printf '%s\n' "$pid" >"$state/sccache-worker.pid"
  chmod 600 "$state/sccache-worker.pid"
  for _ in $(seq 1 300); do
    if ! sudo -n kill -0 "$pid" 2>/dev/null; then
      echo "sccache distributed worker exited during startup" >&2
      sed -n '1,30p' "$results/sccache-worker-stderr" >&2
      exit 1
    fi
    if status="$(curl -fsS --max-time 2 -H 'Accept: application/json' \
      "http://$scheduler_endpoint/api/v1/scheduler/status" 2>/dev/null)" \
      && jq -e '(.num_servers // .SchedulerStatus[1].num_servers // 0) >= 1' <<<"$status" >/dev/null; then
      break
    fi
    sleep 0.1
  done
  if [[ -z "${status:-}" ]] \
    || ! jq -e '(.num_servers // .SchedulerStatus[1].num_servers // 0) >= 1' <<<"$status" >/dev/null; then
    echo "sccache distributed worker did not register with the scheduler" >&2
    exit 1
  fi
  jq -n \
    --arg endpoint "$endpoint" \
    --arg scheduler_endpoint "$scheduler_endpoint" \
    '{schema_version: 1, endpoint: $endpoint, scheduler_endpoint: $scheduler_endpoint}'
  trap - EXIT
}

sccache_dist_client_prepare() {
  [[ "$#" -eq 1 ]] || usage
  local scheduler_endpoint="$1" token cache socket status expected
  cluster_endpoint_valid "$scheduler_endpoint" || usage
  [[ -d "$state" && -d "$results" && ! -e "$sccache_dist_client_config" ]] || {
    echo "sccache distributed client state is unavailable or already prepared: $run_id" >&2
    exit 2
  }
  for tool in jq sccache; do
    command -v "$tool" >/dev/null || {
      echo "sccache distributed client requires $tool" >&2
      exit 2
    }
  done
  expected="sccache $(sccache_pinned_version)"
  [[ "$(sccache --version)" == "$expected" ]] || {
    echo "sccache distributed client requires $expected" >&2
    exit 2
  }
  token="$(sccache_dist_token "$identity/sccache-client-token")"
  cache="$state/sccache-dist-client-cache"
  socket="${TMPDIR:-/tmp}/cargo-rail-sccache-dist-client-$(printf '%s' "$run_id" | sha256sum | cut -c1-16).sock"
  mkdir -p "$cache"
  chmod 700 "$cache"
  printf '%s\n' \
    'server_startup_timeout_ms = 10000' \
    '[dist]' \
    "scheduler_url = \"http://$scheduler_endpoint\"" \
    'toolchains = []' \
    'toolchain_cache_size = 5368709120' \
    "cache_dir = \"$cache\"" \
    '[dist.auth]' \
    'type = "token"' \
    "token = \"$token\"" >"$sccache_dist_client_config"
  chmod 600 "$sccache_dist_client_config"
  env SCCACHE_CONF="$sccache_dist_client_config" SCCACHE_SERVER_UDS="$socket" \
    sccache --stop-server >/dev/null 2>&1 || true
  env SCCACHE_CONF="$sccache_dist_client_config" SCCACHE_SERVER_UDS="$socket" sccache --start-server >/dev/null
  status="$(env SCCACHE_CONF="$sccache_dist_client_config" SCCACHE_SERVER_UDS="$socket" sccache --dist-status)"
  jq -e '.SchedulerStatus[1].num_servers == 1 and .SchedulerStatus[1].num_cpus >= 1' <<<"$status" >/dev/null || {
    echo "sccache distributed client did not observe one worker" >&2
    exit 1
  }
  jq -c '.SchedulerStatus[1] | {schema_version: 1, num_servers, num_cpus, in_progress}' <<<"$status" \
    >"$results/sccache-dist-status.json"
  chmod 600 "$results/sccache-dist-status.json"
  env SCCACHE_CONF="$sccache_dist_client_config" SCCACHE_SERVER_UDS="$socket" \
    sccache --stop-server >/dev/null 2>&1 || true
  jq . "$results/sccache-dist-status.json"
}

sccache_dist_stop() {
  [[ "$#" -eq 1 ]] || usage
  local role="$1" unit_file pid_file unit pid active
  [[ "$role" == scheduler || "$role" == worker ]] || usage
  unit_file="$state/sccache-$role.unit"
  pid_file="$state/sccache-$role.pid"
  [[ -f "$unit_file" && ! -L "$unit_file" && -f "$pid_file" && ! -L "$pid_file" ]] || {
    echo "sccache distributed $role has no launch authority" >&2
    exit 2
  }
  unit="$(<"$unit_file")"
  pid="$(<"$pid_file")"
  [[ "$unit" =~ ^cargo-rail-sccache-(scheduler|worker)-[0-9a-f]{16}\.service$ \
    && "$pid" =~ ^[1-9][0-9]*$ ]] || {
    echo "sccache distributed $role launch authority is invalid" >&2
    exit 2
  }
  [[ "$(sudo -n systemctl show --property=MainPID --value "$unit")" == "$pid" ]] || {
    echo "sccache distributed $role unit changed its main process" >&2
    exit 2
  }
  sudo -n systemctl stop "$unit"
  for _ in $(seq 1 100); do
    active="$(sudo -n systemctl show --property=ActiveState --value "$unit" 2>/dev/null || true)"
    [[ "$active" != active && "$active" != activating && "$active" != deactivating ]] && break
    sleep 0.1
  done
  active="$(sudo -n systemctl show --property=ActiveState --value "$unit" 2>/dev/null || true)"
  [[ "$active" != active && "$active" != activating && "$active" != deactivating ]] || {
    echo "sccache distributed $role did not stop" >&2
    exit 1
  }
  sudo -n kill -0 "$pid" 2>/dev/null && {
    echo "sccache distributed $role process survived unit cleanup" >&2
    exit 1
  }
  if [[ "$role" == worker ]] && ! sudo -n test ! -e "/run/${unit%.service}"; then
    echo "sccache distributed worker runtime directory survived unit cleanup" >&2
    exit 1
  fi
  rm -f -- "$pid_file" "$unit_file"
}

# One eligible action: a single-file, dependency-free, non-linking library whose
# codegen cost scales with the requested function count. Generic bodies keep the
# measured work in real monomorphization instead of source size alone.
write_fixture() {
  local workspace="$1" functions="$2"
  [[ "$functions" =~ ^[1-9][0-9]{0,4}$ ]] || {
    echo "distributed qualification fixture size is invalid: $functions" >&2
    exit 2
  }
  mkdir -p "$workspace/src"
  cat >"$workspace/Cargo.toml" <<'TOML'
[package]
name = "cargo-rail-distributed-qualification"
version = "0.0.0"
edition = "2024"

[lib]
path = "src/lib.rs"

[profile.release]
incremental = false

[workspace]
TOML
  python3 "$repo_root/scripts/bench/distributed-execution-fixture.py" \
    --functions "$functions" --output "$workspace/src/lib.rs"
  cargo generate-lockfile --manifest-path "$workspace/Cargo.toml" --offline --quiet
}

reset_client() {
  [[ "$#" -eq 1 ]] || usage
  local outcome="$1" path
  [[ "$outcome" == remote || "$outcome" == l2 || "$outcome" == fallback ]] || usage
  for path in \
    "$results/$outcome" \
    "$state/cargo-homes/$outcome" \
    "$state/caches/$outcome" \
    "$state/workspace"; do
    [[ "$path" == "$state/"* || "$path" == "$results/"* ]] || {
      echo "distributed qualification reset escaped its run authority: $path" >&2
      exit 2
    }
    rm -rf -- "$path"
  done
}

reset_measure() {
  [[ "$#" -eq 1 ]] || usage
  local attempt="$1" path destination
  [[ "$attempt" =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ && "$attempt" != *..* ]] || usage
  for path in "$measure_results" "$measure_state"; do
    destination="${path%/measure}/measure-failed-$attempt"
    [[ -d "$path" && ! -e "$destination" ]] || {
      echo "distributed measurement reset source or destination is invalid: $path -> $destination" >&2
      exit 2
    }
  done
  for path in "$measure_results" "$measure_state"; do
    destination="${path%/measure}/measure-failed-$attempt"
    mv -- "$path" "$destination"
  done
}

manifest_outputs() {
  local workspace="$1" output="$2"
  local target="$workspace/target"
  : >"$output"
  while IFS= read -r path; do
    [[ -f "$path" && ! -L "$path" && "$path" == "$target/"* ]] || {
      echo "distributed qualification output escaped its target root: $path" >&2
      exit 1
    }
    printf '%s  %s\n' "$(sha256sum "$path" | awk '{print $1}')" "${path#"$target/"}" >>"$output"
  done < <(find "$target" -type f \( -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) -print | LC_ALL=C sort)
  [[ -s "$output" ]] || {
    echo "distributed qualification produced no compiler outputs" >&2
    exit 1
  }
}

run_client() {
  [[ "$#" -eq 4 ]] || usage
  local outcome="$1" endpoint="$2" remote_url="$3" capability_id="$4"
  local directory="$results/$outcome" cargo_home="$state/cargo-homes/$outcome"
  local cache="$state/caches/$outcome" workspace="$state/workspace"
  local -a remote_arguments=()
  [[ "$outcome" == remote || "$outcome" == l2 || "$outcome" == fallback ]] || usage
  [[ "$endpoint" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+:[1-9][0-9]{3,4}$ ]] || usage
  [[ "$capability_id" =~ ^worker-capability-v3:sha256:[0-9a-f]{64}$ ]] || usage
  for file in authority.pem client.pem client.key; do
    require_private_file "$identity/$file"
  done
  [[ ! -e "$directory" && ! -e "$cargo_home" && ! -e "$cache" ]] || {
    echo "distributed qualification client outcome already exists: $outcome" >&2
    exit 2
  }
  mkdir -p "$directory/events" "$directory/seed-events" "$cargo_home" "$cache"
  chmod 700 "$directory" "$directory/events" "$directory/seed-events" "$cargo_home" "$cache"
  if [[ ! -e "$workspace" ]]; then
    write_fixture "$workspace" 256
  fi
  build_binaries
  if [[ "$remote_url" != - ]]; then
    [[ "$remote_url" == r2://* || "$remote_url" == s3://* ]] || usage
    require_private_file "$identity/remote.env"
    # shellcheck disable=SC1091
    source "$identity/remote.env"
    remote_arguments=(--remote "$remote_url" --remote-mode "$([[ "$outcome" == remote ]] && echo read-write || echo read)")
  fi
  env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    CARGO_HOME="$cargo_home" "$binary" rail cache setup \
    --local-dir "$cache" --max-size 2GiB \
    --distributed-endpoint "$endpoint" \
    --distributed-server-name worker.task10.cargo-rail.invalid \
    --distributed-capability "$capability_id" \
    --distributed-authority "$identity/authority.pem" \
    --distributed-client-certificate "$identity/client.pem" \
    --distributed-client-private-key "$identity/client.key" \
    --distributed-policy qualification \
    "${remote_arguments[@]}" -f json >"$directory/seed-setup.json"
  cargo clean --manifest-path "$workspace/Cargo.toml"
  env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER -u CARGO_TARGET_DIR -u OUT_DIR \
    CARGO_HOME="$cargo_home" CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=never \
    CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1 \
  CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY="$directory/seed-events" \
    cargo build --manifest-path "$workspace/Cargo.toml" --release --lib --locked --offline \
    --message-format=json-render-diagnostics >"$directory/seed-stdout" 2>"$directory/seed-stderr"
  # shellcheck disable=SC2016 # jq variables must not be expanded by the shell.
  find "$directory/seed-events" -type f -name 'event-*.json' -print0 | sort -z | xargs -0 jq -s '
    [.[] | select(.action_key | type == "string")] as $actions
    | ($actions | length) == 1
      and all($actions[];
        .status == "miss"
        and (.reason | startswith("environment_selector_not_found;stored_verified_result")))
  ' >/dev/null || {
    echo "distributed qualification did not establish exact local environment authority" >&2
    exit 1
  }
  evict_seeded_native_actions "$cache"
  cargo clean --manifest-path "$workspace/Cargo.toml"
  env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    CARGO_HOME="$cargo_home" "$binary" rail cache setup \
    --local-dir "$cache" --max-size 2GiB \
    --distributed-endpoint "$endpoint" \
    --distributed-server-name worker.task10.cargo-rail.invalid \
    --distributed-capability "$capability_id" \
    --distributed-authority "$identity/authority.pem" \
    --distributed-client-certificate "$identity/client.pem" \
    --distributed-client-private-key "$identity/client.key" \
    --distributed-policy qualification \
    "${remote_arguments[@]}" -f json >"$directory/setup.json"
  python3 "$measure" \
    --cwd "$workspace" \
    --stdout "$directory/stdout" \
    --stderr "$directory/stderr" \
    --output "$directory/timing.json" \
    --unset RUSTC_WRAPPER \
    --unset CARGO_BUILD_RUSTC_WRAPPER \
    --unset RUSTC_WORKSPACE_WRAPPER \
    --unset CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER \
    --unset CARGO_TARGET_DIR \
    --unset OUT_DIR \
    --env "CARGO_HOME=$cargo_home" \
    --env CARGO_INCREMENTAL=0 \
    --env CARGO_TERM_COLOR=never \
    --env CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1 \
    --env "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY=$directory/events" \
    -- cargo build --release --lib --locked --offline --message-format=json-render-diagnostics
  manifest_outputs "$workspace" "$directory/outputs.sha256"
  find "$directory/events" -type f -name 'event-*.json' -print0 | sort -z | xargs -0 jq -s \
    '{
      events: length,
      distributed: ([.[] | select(.status == "hit" and (.reason | startswith("verified_distributed_execution")))] | length),
      distributed_l2_publications: ([.[] | select(.status == "hit" and (.reason | contains("verified_distributed_execution;remote_published")))] | length),
      remote_hits: ([.[] | select(.status == "hit" and .reason == "verified_remote_result")] | length),
      local_fallbacks: ([.[] | select(.status == "miss" and (.reason | contains("stored_verified_result")))] | length),
      reasons: ([.[].reason] | sort)
    }' >"$directory/events.json"
  case "$outcome" in
    remote)
      jq -e --argjson require_l2 "$([[ "$remote_url" == - ]] && echo false || echo true)" \
        '.distributed > 0 and ((.distributed_l2_publications > 0) or ($require_l2 | not))' \
        "$directory/events.json" >/dev/null
      ;;
    l2) jq -e '.remote_hits > 0 and .distributed == 0' "$directory/events.json" >/dev/null ;;
    fallback) jq -e '.local_fallbacks > 0 and .distributed == 0 and .remote_hits == 0' "$directory/events.json" >/dev/null ;;
  esac
  jq -n \
    --arg outcome "$outcome" \
    --slurpfile timing "$directory/timing.json" \
    --slurpfile events "$directory/events.json" \
    '{schema_version: 1, outcome: $outcome, timing: $timing[0], events: $events[0]}' \
    >"$directory/result.json"
  cat "$directory/result.json"
}

report() {
  [[ "$#" -eq 0 ]] || usage
  local worker_events=0
  if [[ -f "$results/worker-events.jsonl" ]]; then
    worker_events="$(jq -s '[.[] | select(.event == "execution_finished" and .status == "success")] | length' \
      "$results/worker-events.jsonl")"
  fi
  jq -n \
    --arg run_id "$run_id" \
    --argjson worker_successes "$worker_events" \
    --slurpfile environment "$results/environment.json" \
    '{schema_version: 1, run_id: $run_id, worker_successes: $worker_successes, environment: $environment[0]}'
}

sccache_pinned_version() {
  sed -n 's/^readonly SCCACHE_VERSION=//p' "$repo_root/scripts/ci/install-tools.sh"
}

measure_fixture_functions() {
  case "$1" in
    small) echo 16 ;;
    large) echo 1024 ;;
    parallel | parallel-check) echo 512 ;;
    *) usage ;;
  esac
}

measure_fixture_crates() {
  case "$1" in
    small | large) echo 1 ;;
    parallel | parallel-check) echo 6 ;;
    *) usage ;;
  esac
}

write_measure_fixture() {
  local workspace="$1" workload="$2" functions crates
  functions="$(measure_fixture_functions "$workload")"
  crates="$(measure_fixture_crates "$workload")"
  if [[ "$crates" == 1 ]]; then
    write_fixture "$workspace" "$functions"
    return
  fi
  python3 "$repo_root/scripts/bench/distributed-execution-fixture.py" \
    --functions "$functions" --crates "$crates" --output "$workspace"
  cargo generate-lockfile --manifest-path "$workspace/Cargo.toml" --offline --quiet
}

# Every measured lane runs the same command with the same environment on the
# same machine. Each lane owns a separate physical workspace, so remap it to
# the distributed protocol's virtual workspace before comparing compiler
# outputs byte-for-byte. Only the compiler acceleration under test differs.
measure_command() {
  local workspace="$1" directory="$2" workload="$3"
  measurement=(
    "$measure"
    --cwd "$workspace"
    --stdout "$directory/stdout"
    --stderr "$directory/stderr"
    --output "$directory/timing.json"
    --unset RUSTC_WRAPPER
    --unset CARGO_BUILD_RUSTC_WRAPPER
    --unset RUSTC_WORKSPACE_WRAPPER
    --unset CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER
    --unset CARGO_TARGET_DIR
    --unset RUSTFLAGS
    --unset CARGO_ENCODED_RUSTFLAGS
    --unset OUT_DIR
    --env CARGO_INCREMENTAL=0
    --env CARGO_TERM_COLOR=never
    --env CARGO_NET_OFFLINE=true
    --env "RUSTFLAGS=--remap-path-prefix=$workspace=/cargo-rail/exec/v3/workspace"
  )
  if [[ "$workload" == parallel-check ]]; then
    build_command=(
      cargo check --release --workspace --lib --locked --offline --jobs 4 --message-format=json-render-diagnostics
    )
  else
    build_command=(
      cargo build --release --workspace --lib --locked --offline --jobs 4 --message-format=json-render-diagnostics
    )
  fi
}

measure_compiler_artifacts() {
  jq -Rrs '[splits("\n") | select(length > 0) | fromjson? | select(.reason == "compiler-artifact")] | length' \
    <"$1/stdout"
}

measure_event_summary() {
  local directory="$1"
  local -a events=()
  mapfile -d '' -t events < <(find "$directory/events" -type f -name 'event-*.json' -print0 | sort -z)
  jq -s '
    def accumulate($blocks):
      $blocks
      | reduce .[] as $sample ({};
          reduce ($sample | to_entries[]) as $phase (.;
            if ($phase.value | type) == "object"
            then .[$phase.key].count = ((.[$phase.key].count // 0) + $phase.value.count)
              | .[$phase.key].elapsed_ns = ((.[$phase.key].elapsed_ns // 0) + $phase.value.elapsed_ns)
            else .[$phase.key] = ((.[$phase.key] // 0) + $phase.value)
            end));
    {
      events: length,
      hits: ([.[] | select(.status == "hit")] | length),
      misses: ([.[] | select(.status == "miss")] | length),
      bypasses: ([.[] | select(.status == "bypassed" or .status == "disabled")] | length),
      distributed: ([.[]
        | select(.status == "hit" and (.reason | startswith("verified_distributed_execution")))] | length),
      remote_hits: ([.[] | select(.status == "hit" and .reason == "verified_remote_result")] | length),
      cold_stores: ([.[] | select(.status == "miss" and (.reason | contains("stored_verified_result")))] | length),
      reasons: ([.[].reason] | sort),
      client_phases: accumulate([.[] | select(.distributed_timing != null) | .distributed_timing | del(.worker)]),
      worker_phases: accumulate([.[] | select(.distributed_timing != null) | .distributed_timing.worker]),
      restore_phases: accumulate([.[] | select(.distributed_timing != null) | .timing | {output_restore}])
    }' "${events[@]}"
}

measure_worker_slice() {
  local directory="$1"
  jq '
    {
      executions: .distributed,
      successes: .distributed,
      queue_ns: (.worker_phases.queue_ns // 0),
      input_ns: (.worker_phases.input_ns // 0),
      compiler_ns: (.worker_phases.compiler_ns // 0),
      result_encode_ns: (.worker_phases.result_encode_ns // 0),
      elapsed_ns: (.worker_phases.elapsed_ns // 0),
      source_bytes: (.worker_phases.source_bytes // 0),
      result_bytes: (.worker_phases.result_bytes // 0)
    }' "$directory/cache.json" >"$directory/worker.json"
}

measure_manifest_outputs() {
  local workspace="$1" output="$2"
  local target="$workspace/target"
  : >"$output"
  while IFS= read -r path; do
    [[ -f "$path" && ! -L "$path" && "$path" == "$target/"* ]] || {
      echo "distributed measurement output escaped its target root: $path" >&2
      return 1
    }
    printf '%s  %s\n' "$(sha256sum "$path" | awk '{print $1}')" "${path#"$target/"}" >>"$output"
  done < <(find "$target" -type f \( -name '*.d' -o -name '*.rmeta' -o -name '*.rlib' \) -print | LC_ALL=C sort)
  [[ -s "$output" ]] || {
    echo "distributed measurement produced no compiler outputs" >&2
    return 1
  }
}

# Dep-info names the physical output directory and is therefore valid only as
# a within-lane repeatability oracle. Remapped rlib/rmeta bytes are the
# cross-lane compiler-artifact contract.
measure_portable_compiler_outputs() {
  local workspace="$1" output="$2"
  local target="$workspace/target"
  : >"$output"
  while IFS= read -r path; do
    [[ -f "$path" && ! -L "$path" && "$path" == "$target/"* ]] || {
      echo "distributed measurement compiler artifact escaped its target root: $path" >&2
      return 1
    }
    printf '%s  %s\n' "$(sha256sum "$path" | awk '{print $1}')" "${path#"$target/"}" >>"$output"
  done < <(find "$target" -type f \( -name '*.rmeta' -o -name '*.rlib' \) -print | LC_ALL=C sort)
  [[ -s "$output" ]] || {
    echo "distributed measurement produced no portable compiler artifacts" >&2
    return 1
  }
}

# The sysroot identity memo is a toolchain-lifetime fact that the native cache
# establishes once and revalidates cheaply. Wiping L1 to force a cold action
# would also wipe it, so each sample re-establishes it before the measured
# window through ordinary product behavior. The warm-up crate has two source
# files, which makes it ineligible for distributed execution, so it never
# reaches a worker. Neither baseline lane re-derives this fact either, so
# charging it to every distributed sample would misattribute the comparison.
write_warmup_fixture() {
  local workspace="$1"
  mkdir -p "$workspace/src"
  cat >"$workspace/Cargo.toml" <<'TOML'
[package]
name = "cargo-rail-distributed-warmup"
version = "0.0.0"
edition = "2024"

[lib]
path = "src/lib.rs"

[profile.release]
incremental = false

[workspace]
TOML
  printf '%s\n' '#![forbid(unsafe_code)]' 'mod extra;' 'pub use extra::warm;' >"$workspace/src/lib.rs"
  printf '%s\n' 'pub fn warm() -> u8 { 1 }' >"$workspace/src/extra.rs"
  cargo generate-lockfile --manifest-path "$workspace/Cargo.toml" --offline --quiet
}

measure_warm_sysroot_memo() {
  local directory="$1" cargo_home="$2"
  local workspace="$measure_state/fixtures/warmup"
  [[ -e "$workspace" ]] || write_warmup_fixture "$workspace"
  rm -rf -- "$workspace/target"
  env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER -u CARGO_TARGET_DIR \
    CARGO_HOME="$cargo_home" CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=never \
    cargo build --manifest-path "$workspace/Cargo.toml" --release --lib --locked --offline \
    >"$directory/warmup.log" 2>&1
}

evict_seeded_native_actions() {
  local cache="$1" root native path
  local -a roots=()
  mapfile -d '' -t roots < <(find "$cache/cargo-rail" -mindepth 1 -maxdepth 1 -type d -name 'local-cas-v2*' -print0)
  [[ "${#roots[@]}" -eq 1 ]] || {
    echo "distributed measurement expected one local CAS root under $cache" >&2
    return 1
  }
  root="${roots[0]}"
  native="$root/native-actions-v2"
  [[ -d "$native" && ! -L "$native" ]] || {
    echo "distributed measurement native action authority is unavailable" >&2
    return 1
  }
  while IFS= read -r -d '' path; do
    [[ -f "$path" && ! -L "$path" && "$path" == "$native/"* ]] || {
      echo "distributed measurement native action entry escaped its authority" >&2
      return 1
    }
    rm -f -- "$path"
  done < <(find "$native" -mindepth 1 -maxdepth 1 -print0)
}

measure_seed_environment_authority() {
  local workload="$1" workspace="$2" directory="$3" cargo_home="$4" cache="$5" expected
  expected="$(measure_fixture_crates "$workload")"
  mkdir -p "$directory/seed-events"
  chmod 700 "$directory/seed-events"
  (
    cd "$workspace"
    env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
      -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER -u CARGO_TARGET_DIR -u OUT_DIR \
      CARGO_HOME="$cargo_home" CARGO_INCREMENTAL=0 CARGO_TERM_COLOR=never CARGO_NET_OFFLINE=true \
      CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1 \
      CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY="$directory/seed-events" \
      RUSTFLAGS="--remap-path-prefix=$workspace=/cargo-rail/exec/v3/workspace" \
      "${build_command[@]}"
  ) >"$directory/seed-stdout" 2>"$directory/seed-stderr"
  # shellcheck disable=SC2016 # jq variables must not be expanded by the shell.
  find "$directory/seed-events" -type f -name 'event-*.json' -print0 | sort -z | xargs -0 jq -s \
    --argjson expected "$expected" '
      [.[] | select(.action_key | type == "string")] as $actions
      | ($actions | length) == $expected
        and all($actions[];
          .status == "miss"
          and (.reason | startswith("environment_selector_not_found;stored_verified_result")))
    ' >/dev/null || {
      echo "distributed measurement did not establish exact local environment authority" >&2
      return 1
    }
  evict_seeded_native_actions "$cache"
  rm -rf -- "$workspace/target"
}

measure_run_cargo_rail() {
  local workload="$1" directory="$2" policy="$3" cargo_home="$4" cache="$5"
  local workspace="$measure_state/fixtures/$workload-cargo-rail-distributed"
  # Every sample must start from an empty L1 so the eligible action reaches the
  # distributed boundary instead of a local hit.
  rm -rf -- "$workspace/target" "$directory" "$cache"
  mkdir -p "$directory/events" "$cargo_home" "$cache"
  chmod 700 "$directory" "$directory/events" "$cargo_home" "$cache"
  env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$cargo_home" "$binary" rail cache setup \
    --local-dir "$cache" --max-size 4GiB -f json >"$directory/seed-setup.json"
  measure_warm_sysroot_memo "$directory" "$cargo_home"
  measure_command "$workspace" "$directory" "$workload"
  env -u RUSTC_WRAPPER -u CARGO_BUILD_RUSTC_WRAPPER -u RUSTC_WORKSPACE_WRAPPER \
    -u CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER CARGO_HOME="$cargo_home" "$binary" rail cache setup \
    --local-dir "$cache" --max-size 4GiB \
    --distributed-endpoint "$measure_endpoint" \
    --distributed-server-name worker.task10.cargo-rail.invalid \
    --distributed-capability "$measure_capability_id" \
    --distributed-authority "$identity/authority.pem" \
    --distributed-client-certificate "$identity/client.pem" \
    --distributed-client-private-key "$identity/client.key" \
    --distributed-policy "$policy" -f json >"$directory/setup.json"
  measure_seed_environment_authority "$workload" "$workspace" "$directory" "$cargo_home" "$cache"
  measurement+=(
    --env "CARGO_HOME=$cargo_home"
    --env CARGO_RAIL_CACHE=__cargo_rail_benchmark_coverage_v1
    --env "CARGO_RAIL_BENCH_NATIVE_COVERAGE_DIRECTORY=$directory/events"
  )
  python3 "${measurement[@]}" -- "${build_command[@]}"
  measure_event_summary "$directory" >"$directory/cache.json"
  measure_worker_slice "$directory"
  measure_manifest_outputs "$workspace" "$directory/outputs.sha256"
  measure_portable_compiler_outputs "$workspace" "$directory/compiler-outputs.sha256"
}

measure_run_cargo_local() {
  local workload="$1" directory="$2"
  local workspace="$measure_state/fixtures/$workload-cargo-local"
  local cargo_home="$measure_state/cargo-homes/$workload-cargo-local"
  rm -rf -- "$workspace/target" "$directory"
  mkdir -p "$directory" "$cargo_home"
  chmod 700 "$directory" "$cargo_home"
  measure_command "$workspace" "$directory" "$workload"
  measurement+=(--env "CARGO_HOME=$cargo_home")
  python3 "${measurement[@]}" -- "${build_command[@]}"
  measure_manifest_outputs "$workspace" "$directory/outputs.sha256"
  measure_portable_compiler_outputs "$workspace" "$directory/compiler-outputs.sha256"
}

measure_sccache_environment() {
  local directory="$1" socket
  socket="${TMPDIR:-/tmp}/cargo-rail-distributed-sccache-$(printf '%s' "$directory" | sha256sum | cut -c1-16).sock"
  sccache_env=(
    "SCCACHE_DIR=$directory/cache"
    "SCCACHE_CACHE_SIZE=8G"
    "SCCACHE_SERVER_UDS=$socket"
    "SCCACHE_ERROR_LOG=$directory/sccache.log"
    "SCCACHE_LOG=warn"
  )
}

measure_stop_sccache() {
  local directory="$1" config="${2:-}"
  measure_sccache_environment "$directory"
  [[ -z "$config" ]] || sccache_env+=("SCCACHE_CONF=$config")
  env "${sccache_env[@]}" "$sccache_bin" --stop-server >/dev/null 2>&1 || true
}

measure_run_sccache() {
  local workload="$1" directory="$2" socket assignment
  local workspace="$measure_state/fixtures/$workload-sccache-local"
  local cargo_home="$measure_state/cargo-homes/$workload-sccache-local"
  rm -rf -- "$workspace/target" "$directory"
  mkdir -p "$directory/cache" "$cargo_home"
  chmod 700 "$directory" "$directory/cache" "$cargo_home"
  measure_sccache_environment "$directory"
  socket="$(printf '%s\n' "${sccache_env[@]}" | sed -n 's/^SCCACHE_SERVER_UDS=//p')"
  rm -f -- "$socket"
  active_sccache_directory="$directory"
  measure_command "$workspace" "$directory" "$workload"
  measurement+=(--env "CARGO_HOME=$cargo_home" --env "RUSTC_WRAPPER=$sccache_bin")
  for assignment in "${sccache_env[@]}"; do
    measurement+=(--env "$assignment")
  done
  python3 "${measurement[@]}" -- "${build_command[@]}"
  env "${sccache_env[@]}" "$sccache_bin" --show-stats --stats-format json >"$directory/cache.json"
  measure_stop_sccache "$directory"
  active_sccache_directory=""
  measure_manifest_outputs "$workspace" "$directory/outputs.sha256"
  measure_portable_compiler_outputs "$workspace" "$directory/compiler-outputs.sha256"
}

measure_run_sccache_distributed() {
  local workload="$1" directory="$2" socket assignment status
  local workspace="$measure_state/fixtures/$workload-sccache-distributed"
  local cargo_home="$measure_state/cargo-homes/$workload-sccache-distributed"
  rm -rf -- "$workspace/target" "$directory"
  mkdir -p "$directory/cache" "$cargo_home"
  chmod 700 "$directory" "$directory/cache" "$cargo_home"
  measure_sccache_environment "$directory"
  sccache_env+=("SCCACHE_CONF=$sccache_dist_client_config")
  socket="$(printf '%s\n' "${sccache_env[@]}" | sed -n 's/^SCCACHE_SERVER_UDS=//p')"
  rm -f -- "$socket"
  active_sccache_directory="$directory"
  measure_stop_sccache "$directory" "$sccache_dist_client_config"
  env "${sccache_env[@]}" "$sccache_bin" --start-server >/dev/null
  status="$(env "${sccache_env[@]}" "$sccache_bin" --dist-status)"
  jq -e '.SchedulerStatus[1].num_servers == 1 and .SchedulerStatus[1].num_cpus >= 1' <<<"$status" >/dev/null || {
    echo "sccache distributed measurement lost its worker" >&2
    return 1
  }
  measure_command "$workspace" "$directory" "$workload"
  measurement+=(--env "CARGO_HOME=$cargo_home" --env "RUSTC_WRAPPER=$sccache_bin")
  for assignment in "${sccache_env[@]}"; do
    measurement+=(--env "$assignment")
  done
  python3 "${measurement[@]}" -- "${build_command[@]}"
  env "${sccache_env[@]}" "$sccache_bin" --show-stats --stats-format json >"$directory/cache.json"
  measure_stop_sccache "$directory" "$sccache_dist_client_config"
  active_sccache_directory=""
  measure_manifest_outputs "$workspace" "$directory/outputs.sha256"
  measure_portable_compiler_outputs "$workspace" "$directory/compiler-outputs.sha256"
}

measure_lane() {
  local workload="$1" lane="$2" directory="$3"
  case "$lane" in
    cargo-rail-distributed)
      measure_run_cargo_rail "$workload" "$directory" qualification \
        "$measure_state/cargo-homes/$workload-cargo-rail-distributed" \
        "$measure_state/caches/$workload-cargo-rail-distributed"
      ;;
    cargo-local) measure_run_cargo_local "$workload" "$directory" ;;
    sccache-local) measure_run_sccache "$workload" "$directory" ;;
    sccache-distributed) measure_run_sccache_distributed "$workload" "$directory" ;;
    *) usage ;;
  esac
}

measure_lane_outcome_valid() {
  local workload="$1" lane="$2" directory="$3" expected
  expected="$(measure_fixture_crates "$workload")"
  [[ "$(measure_compiler_artifacts "$directory")" == "$expected" ]] || return 1
  case "$lane" in
    cargo-rail-distributed)
      jq -e --arg workload "$workload" --argjson expected "$expected" '
        .remote_hits == 0
        and .distributed + .cold_stores == $expected
        and (if ($workload == "parallel" or $workload == "parallel-check")
          then .distributed >= 1 and .cold_stores >= 1
          else .distributed == $expected and .cold_stores == 0
          end)
      ' "$directory/cache.json" >/dev/null \
        && jq -e --argjson expected "$(jq '.distributed' "$directory/cache.json")" \
          '.executions == $expected and .successes == $expected' "$directory/worker.json" >/dev/null
      ;;
    cargo-local)
      [[ ! -e "$directory/events" && ! -e "$directory/cache.json" ]]
      ;;
    sccache-local)
      jq -e --argjson expected "$expected" '
        .stats.compilations == $expected
        and ([.stats.cache_hits.counts[]?] | add // 0) == 0
        and .stats.cache_read_errors == 0
        and .stats.cache_write_errors == 0
        and (.stats.cache_errors.counts | length) == 0
      ' "$directory/cache.json" >/dev/null
      ;;
    sccache-distributed)
      jq -e --arg workload "$workload" --argjson expected "$expected" '
        .stats.compilations == $expected
        and .stats.compile_fails == 0
        and ([.stats.cache_misses.counts[]?] | add // 0) == $expected
        and .stats.cache_writes == $expected
        and (if ($workload == "parallel" or $workload == "parallel-check")
          then ([.stats.dist_compiles[]?] | add // 0) >= 1
            and ([.stats.dist_compiles[]?] | add // 0) + .stats.dist_errors == $expected
          else ([.stats.dist_compiles[]?] | add // 0) == $expected and .stats.dist_errors == 0
          end)
        and ([.stats.cache_hits.counts[]?] | add // 0) == 0
        and .stats.cache_read_errors == 0
        and .stats.cache_write_errors == 0
        and (.stats.cache_errors.counts | length) == 0
      ' "$directory/cache.json" >/dev/null
      ;;
    *) return 2 ;;
  esac
}

measure_sample() {
  local workload="$1" lane="$2" round="$3"
  local directory="$measure_results/raw/$workload/round-$round/$lane"
  local outcome_ok=false outputs_identical=false accepted=false
  measure_lane "$workload" "$lane" "$directory"
  measure_lane_outcome_valid "$workload" "$lane" "$directory" && outcome_ok=true
  cmp -s "$measure_results/seed/$workload/$lane/outputs.sha256" "$directory/outputs.sha256" && outputs_identical=true
  [[ "$outcome_ok" == true && "$outputs_identical" == true ]] && accepted=true
  jq -n \
    --arg sample_id "$workload-$round-$lane" \
    --arg workload "$workload" \
    --arg lane "$lane" \
    --argjson round "$round" \
    --argjson accepted "$accepted" \
    --argjson outcome_ok "$outcome_ok" \
    --argjson outputs_identical "$outputs_identical" \
    --slurpfile timing "$directory/timing.json" \
    --argjson cache "$(measure_optional_json "$directory/cache.json")" \
    --argjson worker "$(measure_optional_json "$directory/worker.json")" \
    '{
      schema_version: 1,
      sample_id: $sample_id,
      workload: $workload,
      lane: $lane,
      round: $round,
      accepted: $accepted,
      lane_outcome_valid: $outcome_ok,
      outputs_identical_to_lane_authority: $outputs_identical,
      measurement: $timing[0],
      cache: $cache,
      worker: $worker
    }' >"$directory/sample.json"
  [[ "$accepted" == true ]] || {
    echo "distributed measurement sample rejected: $workload/round-$round/$lane" >&2
    jq '{lane_outcome_valid, outputs_identical_to_lane_authority, cache, worker}' "$directory/sample.json" >&2
    return 1
  }
}

measure_optional_json() {
  if [[ -f "$1" ]]; then cat "$1"; else echo null; fi
}

# Automatic placement owns retention; the sampling rounds deliberately use the
# qualification policy. Three qualification rounds publish remote observations
# and four automatic rounds publish local ones, so the final decision is checked
# against the measured medians instead of an assumed direction.
measure_retention() {
  local workload="$1" retention_expected="$2" round
  local directory="$measure_results/retention/$workload"
  local cargo_home="$measure_state/cargo-homes/$workload-retention"
  local cache="$measure_state/caches/$workload-retention"
  rm -rf -- "$directory" "$cargo_home" "$cache"
  mkdir -p "$directory"
  chmod 700 "$directory"
  for round in 1 2 3; do
    measure_run_cargo_rail "$workload" "$directory/qualification-$round" qualification "$cargo_home" "$cache"
    jq -e '.distributed == 1' "$directory/qualification-$round/cache.json" >/dev/null || {
      echo "distributed retention qualification round did not delegate: $workload/$round" >&2
      return 1
    }
  done
  for round in 1 2 3 4; do
    measure_run_cargo_rail "$workload" "$directory/automatic-$round" automatic "$cargo_home" "$cache"
  done
  env CARGO_HOME="$cargo_home" "$binary" rail cache status --scope local -f json >"$directory/status.json"
  jq -n \
    --arg workload "$workload" \
    --argjson retention_expected "$retention_expected" \
    --slurpfile status "$directory/status.json" \
    --argjson automatic "$(jq -s '[.[] | .distributed] | add' "$directory"/automatic-*/cache.json)" \
    --argjson qualification "$(jq -s '[.[] | .distributed] | add' "$directory"/qualification-*/cache.json)" \
    '($status[0].status.installation.distributed_placement_history) as $placement
    | {
        schema_version: 1,
        workload: $workload,
        retention_expected: $retention_expected,
        qualification_delegations: $qualification,
        automatic_delegations: $automatic,
        placement: $placement,
        passed: (
          $qualification == 3
          and $placement != null
          and $placement.remote_observations >= 3
          and $placement.local_observations >= 3
          and (if $retention_expected then $automatic == 0 else true end)
        )
      }' >"$directory/retention.json"
  jq -e '.passed' "$directory/retention.json" >/dev/null || {
    echo "distributed automatic placement retention gate failed: $workload" >&2
    jq . "$directory/retention.json" >&2
    return 1
  }
}

measure_summary() {
  find "$measure_results/raw" -type f -name sample.json -print0 | sort -z | xargs -0 jq -sc . \
    >"$measure_results/samples.json"
  jq -n \
    --argjson rounds "$measure_rounds" \
    --argjson vcpus "$(nproc)" \
    --arg sccache "$sccache_version" \
    --arg endpoint_network "$measure_endpoint_network" \
    --slurpfile samples "$measure_results/samples.json" '
    def quantile($values; $p):
      ($values | sort) as $sorted
      | if ($sorted | length) == 0
        then null
        else $sorted[((($sorted | length) * $p | ceil) - 1)]
        end;
    def metric($workload; $lane):
      [$samples[0][] | select(.accepted and .workload == $workload and .lane == $lane)] as $selected
      | [$selected[].measurement.elapsed_seconds] as $elapsed
      | {
          workload: $workload,
          lane: $lane,
          accepted_samples: ($selected | length),
          p50_elapsed_seconds: quantile($elapsed; 0.50),
          p95_elapsed_seconds: quantile($elapsed; 0.95),
          mean_elapsed_seconds: (if ($elapsed | length) == 0 then null else ($elapsed | add / length) end),
          min_elapsed_seconds: ($elapsed | min),
          max_elapsed_seconds: ($elapsed | max),
          total_user_seconds: ([$selected[].measurement.user_seconds] | add),
          total_system_seconds: ([$selected[].measurement.system_seconds] | add),
          max_rss_bytes: ([$selected[].measurement.max_rss_bytes] | max)
        };
    def reduction($baseline; $candidate):
      if $baseline == null or $candidate == null or $baseline == 0 then null
      else 100 * ($baseline - $candidate) / $baseline end;
    def phases($workload):
      [$samples[0][]
        | select(.accepted and .workload == $workload and .lane == "cargo-rail-distributed")] as $selected
      | ($selected | length) as $count
      | if $count == 0 then null else
        {
          samples: $count,
          client: ([$selected[].cache.client_phases]
            | reduce .[] as $sample ({};
                reduce ($sample | to_entries[]) as $phase (.;
                  if ($phase.value | type) == "object"
                  then .[$phase.key] = ((.[$phase.key] // 0) + $phase.value.elapsed_ns)
                  else .[$phase.key] = ((.[$phase.key] // 0) + $phase.value)
                  end))),
          restore_ns: ([$selected[].cache.restore_phases.output_restore.elapsed_ns] | add // 0),
          worker: {
            queue_ns: ([$selected[].worker.queue_ns] | add // 0),
            input_ns: ([$selected[].worker.input_ns] | add // 0),
            compiler_ns: ([$selected[].worker.compiler_ns] | add // 0),
            result_encode_ns: ([$selected[].worker.result_encode_ns] | add // 0),
            elapsed_ns: ([$selected[].worker.elapsed_ns] | add // 0)
          },
          measured_elapsed_ns: (([$selected[].measurement.elapsed_seconds] | add) * 1000000000 | floor)
        }
        end;
    ["small", "large", "parallel", "parallel-check"] as $workloads
    | ["cargo-rail-distributed", "cargo-local", "sccache-local", "sccache-distributed"] as $lanes
    | [$workloads[] as $workload | $lanes[] as $lane | metric($workload; $lane)] as $metrics
    | {
        schema_version: 1,
        evidence_class: "operator_bounded_critical_path_qualification",
        bounded_worst_case_qualification: true,
        statistical_distribution_claim: false,
        sample_interpretation:
          "three accepted interleaved rounds are the qualification corpus; p95 is the maximum observed sample, not an estimated population percentile",
        rounds: $rounds,
        maximum_rounds: 3,
        interleaving: "deterministic rotation by workload and round",
        client_vcpus: $vcpus,
        cargo_jobs: 4,
        worker_machines: 1,
        worker_max_concurrency: 2,
        sccache_scheduler_machines: 1,
        resource_note:
          "each parallel workload is a six-crate dependency DAG with three initially ready producers and three dependent consumers; both distributed lanes use one equal-shape worker, while the pinned sccache lane also uses one scheduler machine whose cost is reported separately",
        endpoint_network: $endpoint_network,
        sccache: $sccache,
        sccache_lane: "pinned sccache distributed client and server with a warm toolchain cache and cold result cache",
        accepted_samples: ([$samples[0][] | select(.accepted)] | length),
        rejected_samples: ([$samples[0][] | select(.accepted | not)] | length),
        metrics: $metrics,
        critical_path: [$workloads[] | {workload: ., phases: phases(.)}],
        comparisons: [
          $workloads[] as $workload
          | ($metrics[] | select(.workload == $workload and .lane == "cargo-rail-distributed")) as $distributed
          | ($metrics[] | select(.workload == $workload and .lane == "cargo-local")) as $local
          | ($metrics[] | select(.workload == $workload and .lane == "sccache-local")) as $sccache_lane
          | ($metrics[] | select(.workload == $workload and .lane == "sccache-distributed")) as $sccache_dist
          | {
              workload: $workload,
              distributed_vs_cargo_local_p50_reduction_percent:
                reduction($local.p50_elapsed_seconds; $distributed.p50_elapsed_seconds),
              distributed_vs_cargo_local_p95_reduction_percent:
                reduction($local.p95_elapsed_seconds; $distributed.p95_elapsed_seconds),
              distributed_vs_sccache_local_p50_reduction_percent:
                reduction($sccache_lane.p50_elapsed_seconds; $distributed.p50_elapsed_seconds),
              distributed_vs_sccache_local_p95_reduction_percent:
                reduction($sccache_lane.p95_elapsed_seconds; $distributed.p95_elapsed_seconds),
              distributed_vs_sccache_distributed_p50_reduction_percent:
                reduction($sccache_dist.p50_elapsed_seconds; $distributed.p50_elapsed_seconds),
              distributed_vs_sccache_distributed_p95_reduction_percent:
                reduction($sccache_dist.p95_elapsed_seconds; $distributed.p95_elapsed_seconds),
              distributed_retention_expected:
                ($distributed.p50_elapsed_seconds >= $local.p50_elapsed_seconds)
            }
        ]
      }
    | .all_samples_accepted = (.accepted_samples == (16 * $rounds) and .rejected_samples == 0)
    ' >"$measure_results/summary.json"
}

measure() {
  [[ "$#" -eq 3 ]] || usage
  measure_rounds="$1"
  measure_endpoint="$2"
  measure_capability_id="$3"
  local workload lane round workload_index offset position tool retention_expected
  local -a workloads=(small large parallel parallel-check)
  local -a lanes=(cargo-rail-distributed cargo-local sccache-local sccache-distributed)
  # The operator caps qualification at three interleaved rounds. At that size,
  # p95 is the maximum observed sample rather than a population estimate; the
  # summary records both the bounded verdict and that statistical limit.
  [[ "$measure_rounds" =~ ^[1-3]$ ]] || usage
  [[ "$measure_endpoint" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+:[1-9][0-9]{3,4}$ ]] || usage
  [[ "$measure_capability_id" =~ ^worker-capability-v3:sha256:[0-9a-f]{64}$ ]] || usage
  for tool in cargo jq python3 sccache nproc; do
    command -v "$tool" >/dev/null || {
      echo "distributed measurement requires $tool" >&2
      exit 2
    }
  done
  for file in authority.pem client.pem client.key; do
    require_private_file "$identity/$file"
  done
  require_private_file "$sccache_dist_client_config"
  [[ ! -e "$measure_results" && ! -e "$measure_state" ]] || {
    echo "distributed measurement state already exists: $run_id" >&2
    exit 2
  }
  sccache_bin="${SCCACHE_BIN:-$(command -v sccache)}"
  sccache_version="$("$sccache_bin" --version)"
  [[ "$sccache_version" == "sccache $(sccache_pinned_version)" ]] || {
    echo "distributed measurement requires pinned sccache $(sccache_pinned_version); found $sccache_version" >&2
    exit 2
  }
  case "$measure_endpoint" in
    10.*.*.* | 172.1[6-9].* | 172.2[0-9].* | 172.3[01].* | 192.168.*) measure_endpoint_network="private" ;;
    100.*) measure_endpoint_network="tailscale" ;;
    *) measure_endpoint_network="other" ;;
  esac
  build_binaries
  mkdir -p "$measure_results/seed" "$measure_results/raw" "$measure_state/fixtures" \
    "$measure_state/cargo-homes" "$measure_state/caches"
  chmod 700 "$measure_results" "$measure_state" "$measure_state/fixtures" "$measure_state/cargo-homes" \
    "$measure_state/caches"

  for workload in "${workloads[@]}"; do
    for lane in "${lanes[@]}"; do
      write_measure_fixture "$measure_state/fixtures/$workload-$lane" "$workload"
    done
  done
  for workload in "${workloads[@]}"; do
    for lane in "${lanes[@]}"; do
      measure_lane "$workload" "$lane" "$measure_results/seed/$workload/$lane"
      measure_lane_outcome_valid "$workload" "$lane" "$measure_results/seed/$workload/$lane" || {
        echo "distributed measurement seed lane is invalid: $workload/$lane" >&2
        exit 1
      }
    done
    for lane in cargo-rail-distributed sccache-local sccache-distributed; do
      cmp -s "$measure_results/seed/$workload/cargo-local/compiler-outputs.sha256" \
        "$measure_results/seed/$workload/$lane/compiler-outputs.sha256" || {
        echo "distributed measurement lanes disagree on compiler artifact bytes: $workload/$lane" >&2
        exit 1
      }
    done
  done

  for ((round = 1; round <= measure_rounds; round++)); do
    for workload_index in "${!workloads[@]}"; do
      workload="${workloads[$workload_index]}"
      offset=$(((round + workload_index - 1) % ${#lanes[@]}))
      for ((position = 0; position < ${#lanes[@]}; position++)); do
        lane="${lanes[$(((offset + position) % ${#lanes[@]}))]}"
        measure_sample "$workload" "$lane" "$round"
      done
    done
  done

  measure_summary
  for workload in small large; do
    retention_expected="$(jq -r --arg workload "$workload" \
      '.comparisons[] | select(.workload == $workload) | .distributed_retention_expected' \
      "$measure_results/summary.json")"
    measure_retention "$workload" "$retention_expected"
  done
  find "$measure_results/retention" -type f -name retention.json -print0 | sort -z | xargs -0 jq -sc . \
    >"$measure_results/retention.json"
  measure_environment
  jq . "$measure_results/summary.json"
  echo "distributed measurement result: $measure_results"
  # A measured loss is a valid qualification finding, so speed does not decide
  # the exit status. Rejected samples and retention failures still do.
  jq -e '.all_samples_accepted' "$measure_results/summary.json" >/dev/null || {
    echo "distributed measurement rejected at least one sample" >&2
    exit 1
  }
}

measure_environment() {
  local harness="$measure_results/harness-sha256.txt"
  git -C "$repo_root" status --porcelain=v1 --untracked-files=all >"$measure_results/worktree-status.txt"
  printf '%s  %s\n' "$(sha256sum "${BASH_SOURCE[0]}" | awk '{print $1}')" \
    scripts/ci/qualify-distributed-execution-node.sh >"$harness"
  printf '%s  %s\n' "$(sha256sum "$measure" | awk '{print $1}')" scripts/bench/measure-command.py >>"$harness"
  printf '%s  %s\n' "$(sha256sum "$repo_root/scripts/bench/distributed-execution-fixture.py" | awk '{print $1}')" \
    scripts/bench/distributed-execution-fixture.py >>"$harness"
  jq -n \
    --arg generated_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg run_id "$run_id" \
    --arg commit "$(git -C "$repo_root" rev-parse HEAD)" \
    --arg worktree_status_sha256 "$(sha256sum "$measure_results/worktree-status.txt" | awk '{print $1}')" \
    --arg client_binary_sha256 "$(sha256sum "$binary" | awk '{print $1}')" \
    --arg worker_binary_sha256 "$(sha256sum "$worker" | awk '{print $1}')" \
    --arg harness_sha256 "$(sha256sum "$harness" | awk '{print $1}')" \
    --arg rustc "$(rustc -vV)" \
    --arg cargo "$(cargo -Vv)" \
    --arg host "$(uname -a)" \
    --arg endpoint "$measure_endpoint" \
    --arg capability_id "$measure_capability_id" \
    --arg instance_type "${DEV_MACHINE_INSTANCE_TYPE:-unknown}" \
    --arg target "${DEV_MACHINE_TARGET:-unknown}" \
    --arg sccache "$sccache_version" \
    '{
      schema_version: 1,
      generated_at: $generated_at,
      run_id: $run_id,
      repository_commit: $commit,
      worktree_status_sha256: $worktree_status_sha256,
      client_binary_sha256: $client_binary_sha256,
      worker_binary_sha256: $worker_binary_sha256,
      benchmark_harness_sha256: $harness_sha256,
      rustc: $rustc,
      cargo: $cargo,
      host: $host,
      dev_machine_target: $target,
      instance_type: $instance_type,
      worker_endpoint: $endpoint,
      worker_capability_id: $capability_id,
      sccache: $sccache
    }' >"$measure_results/environment.json"
}

case "$phase" in
  prepare) shift 2; prepare "$@" ;;
  seal-identity) shift 2; seal_identity "$@" ;;
  build) shift 2; build "$@" ;;
  resources) shift 2; qualify_resources "$@" ;;
  worker-start) shift 2; worker_start "$@" ;;
  worker-stop) shift 2; worker_stop "$@" ;;
  sccache-scheduler-start) shift 2; sccache_dist_scheduler_start "$@" ;;
  sccache-worker-start) shift 2; sccache_dist_worker_start "$@" ;;
  sccache-client-prepare) shift 2; sccache_dist_client_prepare "$@" ;;
  sccache-stop) shift 2; sccache_dist_stop "$@" ;;
  reset-client) shift 2; reset_client "$@" ;;
  reset-measure) shift 2; reset_measure "$@" ;;
  run) shift 2; run_client "$@" ;;
  measure)
    shift 2
    mkdir -p "$(dirname "$measure_lock")"
    if ! mkdir "$measure_lock" 2>/dev/null; then
      echo "distributed measurement is already running: $measure_lock" >&2
      exit 2
    fi
    printf '%s\n' "$$" >"$measure_lock/pid"
    trap release_measurement EXIT
    measure "$@"
    ;;
  report) shift 2; report "$@" ;;
  *) usage ;;
esac
