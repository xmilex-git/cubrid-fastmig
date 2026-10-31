# The two-node HA pair this host can run: two rootless podman containers on a private network (workspace#590).
# Sourced by e2e/ops/ha/run.sh; defines functions only.
#
# Why containers: CUBRID HA names its nodes by host name (cubrid_ha.conf ha_node_list, `hostname` on each node) and runs
# one cub_master per host on a fixed ha_port_id, so two nodes cannot share one host name or one network namespace. The
# workspace ctp-run skill runs its ha_shell pairs the same way (ctp_run.sh launch_ha_slave). Each container has its own
# network and IPC namespace, so the conf's ports and SHM ids never collide with servers on the host: no port claim.
#
# Paths: the run directory is bind-mounted at its own host path, so a DB's _vinf, databases.txt and volume headers name
# the same paths inside the node and on the host, where cubrid-fastmig reads the stopped DB. CUBRID runs as the
# container's root, which is this user on the host (rootless), so the host reads every file the node writes.
# Each node also gets <run dir>/<host>/private at /fm590-private: one absolute path, a different directory per node, as
# the same path on two hosts is (the official pair's LOB files, official_pair.sh).
# Cores: core_pattern is host-global and resolved inside the crashing process's mount namespace; /data/core is bound at
# the same path, so a node's core lands where the host's cores do.

# cubridci image of the ctp-run skill (digest pinned there: .agents/skills/ctp-run/scripts/ctp_run.sh DEFAULT_IMAGE_DIGEST)
HA_IMAGE=${FASTMIG_HA_IMAGE:-docker.io/cubridci/cubridci@sha256:79a344fc7664af48cd13b4b01bc54c059dd92ac2be812f2e4d9802234e06fb7e}
HA_GROUP=fmha
HA_HOSTS=(fmmaster fmslave)   # node host names; index 0 starts heartbeat first and so becomes master

# pair_up <run id> <run dir>: network with its own subnet, both nodes idle (PID 1 reaps, nothing of CUBRID runs yet).
# Node addresses are fixed (.11, .12) and given to both nodes with --add-host, so name resolution needs no DNS plugin.
pair_up() {
  local id=$1 dir=$2 prefix i
  HA_NET=fm590_${id}_net
  HA_NODES=(fm590_${id}_${HA_HOSTS[0]} fm590_${id}_${HA_HOSTS[1]})
  podman network create "$HA_NET" >/dev/null || return 1
  prefix=$(podman network inspect "$HA_NET" --format '{{range .Subnets}}{{.Subnet}}{{end}}' | cut -d. -f1-3)
  local -a hosts=(--add-host "${HA_HOSTS[0]}:$prefix.11" --add-host "${HA_HOSTS[1]}:$prefix.12")
  for i in 0 1; do
    mkdir -p "$dir/${HA_HOSTS[i]}/private" || return 1
    podman run -d --name "${HA_NODES[i]}" --hostname "${HA_HOSTS[i]}" --network "$HA_NET" --ip "$prefix.1$((i + 1))" \
      "${hosts[@]}" --ipc=private --cgroupns=private --shm-size=4g --ulimit core=-1 \
      -v "$dir:$dir:rw" -v "$dir/${HA_HOSTS[i]}/private:/fm590-private:rw" \
      -v "$(readlink -f /data/core):/data/core:rw" -e TZ=Asia/Seoul -e LC_ALL=en_US \
      --entrypoint /bin/bash "$HA_IMAGE" -c 'while :; do sleep 3600 & wait $!; done' >/dev/null || return 1
  done
}

# node_sh <0|1> <install> <databases dir> <shell text>: runs the text in the node under that CUBRID install.
# stdin is /dev/null and the caller redirects stdout/stderr to a file: `cubrid server|heartbeat start` hangs when its
# daemons inherit a pipe (cubrid-server-control skill).
# CUBRID_TMP is short and inside the node's own filesystem: the default $CUBRID/tmp under the run dir makes the PL
# server's unix socket path too long, and the PL server (so cub_server) fails to start (e2e/run.sh does the same).
node_sh() {
  local i=$1 inst=$2 dbs=$3; shift 3
  podman exec -i -w "$dbs" -e CUBRID="$inst" -e CUBRID_DATABASES="$dbs" -e CUBRID_TMP=/var/tmp/cubrid \
    -e PATH="$inst/bin:/usr/bin:/bin" -e LD_LIBRARY_PATH="$inst/lib" "${HA_NODES[i]}" \
    bash -c "mkdir -p /var/tmp/cubrid && $*" </dev/null
}

# pair_down: removes both nodes and the network (whatever still runs in them dies with the container)
pair_down() {
  local n
  for n in "${HA_NODES[@]}"; do podman rm -f -t 10 "$n" >/dev/null 2>&1; done
  podman network rm "$HA_NET" >/dev/null 2>&1
  return 0
}
