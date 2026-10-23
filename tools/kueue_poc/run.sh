#!/usr/bin/env bash
set -Eeuo pipefail

CLUSTER=lw-kueue-poc
CONTEXT=kind-${CLUSTER}
NAMESPACE=kueue-poc
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
FIXTURE=${ROOT}/tools/kueue_poc/fixture.yaml
KUEUE_VERSION=v0.20.2
KUEUE_IMAGE="registry.k8s.io/kueue/kueue:${KUEUE_VERSION}@sha256:36cf91ae547395836c75c279d38266ed0c5b2b42ea7c82aaf3f990c7a89dfc1b"
KUEUE_LOCAL_IMAGE=kueue-poc:${KUEUE_VERSION}
BUSYBOX_IMAGE="busybox:1.36.1@sha256:73aaf090f3d85aa34ee199857f03fa3a95c8ede2ffd4cc2cdb5b94e566b11662"
BUSYBOX_LOCAL_IMAGE=busybox-poc:1.36.1
MANIFEST_URL="https://github.com/kubernetes-sigs/kueue/releases/download/${KUEUE_VERSION}/manifests.yaml"
MANIFEST_SHA256=1ab14ecd590575ac57f50fa4cbcf529766bc2c32ba974bda1614381281ec52b4
TERMINATION_GRACE_SECONDS=20
TMP_DIR=$(mktemp -d)
MANIFEST=${TMP_DIR}/kueue-manifests.yaml

cleanup() {
  kind delete cluster --name "${CLUSTER}" >/dev/null 2>&1 || true
  rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

printf '%s\n' "== create ${CLUSTER} =="
kind create cluster --name "${CLUSTER}" --image kindest/node:v1.35.0 --wait 120s

printf '%s\n' '== fetch and verify Kueue manifest =='
curl --fail --location --retry 5 --retry-delay 3 --connect-timeout 20 --max-time 300 \
  --output "${MANIFEST}" "${MANIFEST_URL}"
printf '%s  %s\n' "${MANIFEST_SHA256}" "${MANIFEST}" | sha256sum --check --strict

printf '%s\n' '== pull and import digest-pinned images =='
docker pull --platform=linux/amd64 "${KUEUE_IMAGE}"
docker tag "registry.k8s.io/kueue/kueue:${KUEUE_VERSION}" "${KUEUE_LOCAL_IMAGE}"
docker pull --platform=linux/amd64 "${BUSYBOX_IMAGE}"
docker tag busybox:1.36.1 "${BUSYBOX_LOCAL_IMAGE}"
NODE=$(kind get nodes --name "${CLUSTER}" | awk 'NR==1 {print}')
for image in "${KUEUE_LOCAL_IMAGE}" "${BUSYBOX_LOCAL_IMAGE}"; do
  docker save "${image}" | docker exec -i "${NODE}" \
    ctr --namespace=k8s.io images import --digests --snapshotter=overlayfs -
done

printf '%s\n' '== install Kueue =='
kubectl --context "${CONTEXT}" apply --server-side --filename "${MANIFEST}"
kubectl --context "${CONTEXT}" --namespace kueue-system \
  set image deployment/kueue-controller-manager manager="${KUEUE_LOCAL_IMAGE}"
kubectl --context "${CONTEXT}" --namespace kueue-system \
  patch deployment/kueue-controller-manager --type=strategic \
  --patch '{"spec":{"template":{"spec":{"containers":[{"name":"manager","imagePullPolicy":"Never"}]}}}}'
kubectl --context "${CONTEXT}" --namespace kueue-system \
  rollout status deployment/kueue-controller-manager --timeout=180s
sleep 10
kubectl --context "${CONTEXT}" version --short 2>/dev/null || kubectl --context "${CONTEXT}" version
kubectl --context "${CONTEXT}" get crd workloads.kueue.x-k8s.io \
  --output=jsonpath='{.spec.versions[*].name}{" storage="}{range .spec.versions[*]}{.storage}{" "}{end}{"\\n"}'

printf '%s\n' '== apply quota and interactive fixture =='
kubectl --context "${CONTEXT}" apply --filename "${FIXTURE}"
kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  wait --for=jsonpath='{.status.conditions[?(@.type=="Active")].status}'=True \
  localqueue/poc-queue --timeout=60s
kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  wait --for=condition=available deployment/low-interactive --timeout=120s
LOW_NAME=$(kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  get pods --selector=app=low-interactive --output=jsonpath='{.items[0].metadata.name}')
LOW_UID=$(kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  get pod "${LOW_NAME}" --output=jsonpath='{.metadata.uid}')
printf 'low admitted name=%s uid=%s\n' "${LOW_NAME}" "${LOW_UID}"

printf '%s\n' '== evaluate native Kueue preemption and SIGTERM grace =='
kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  scale deployment/high-interactive --replicas=1
DELETION_SEEN=0
DELETION_LOCAL_TS=0
TERM_LOG=''
for _ in $(seq 1 120); do
  if ! kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
    get pod "${LOW_NAME}" >/dev/null 2>&1; then
    break
  fi
  DELETION_TS=$(kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
    get pod "${LOW_NAME}" --output=jsonpath='{.metadata.deletionTimestamp}' 2>/dev/null || true)
  if [[ -n "${DELETION_TS}" ]]; then
    DELETION_SEEN=1
    DELETION_LOCAL_TS=$(date +%s)
    TERM_LOG=$(kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
      logs "${LOW_NAME}" --container=shell --ignore-errors 2>/dev/null || true)
    break
  fi
  sleep 1
done
[[ "${DELETION_SEEN}" -eq 1 ]]
printf 'Kueue eviction set deletionTimestamp; SIGTERM log=%s\n' "${TERM_LOG:-<not observed before object deletion>}"

GONE_LOCAL_TS=0
for _ in $(seq 1 120); do
  if ! kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
    get pod "${LOW_NAME}" >/dev/null 2>&1; then
    GONE_LOCAL_TS=$(date +%s)
    break
  fi
  sleep 1
done
[[ "${GONE_LOCAL_TS}" -gt 0 ]]
ELAPSED=$((GONE_LOCAL_TS - DELETION_LOCAL_TS))
printf 'Pod object removal observed after approximately %ss; terminationGracePeriodSeconds=%ss\n' \
  "${ELAPSED}" "${TERMINATION_GRACE_SECONDS}"

kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  wait --for=condition=available deployment/high-interactive --timeout=180s
HIGH_PHASE=$(kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  get pods --selector=app=high-interactive --output=jsonpath='{.items[0].status.phase}')
[[ "${HIGH_PHASE}" == Running ]]
LOW_STATE=$(kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  get pods --selector=app=low-interactive \
  --output=jsonpath='{range .items[*]}{.metadata.name}{" "}{.metadata.uid}{" "}{.status.phase}{" "}{.spec.schedulingGates[0].name}{"\\n"}{end}')
printf 'high phase=%s; replacement low state=%s\n' "${HIGH_PHASE}" "${LOW_STATE:-<none>}"
printf '%s\n' "${LOW_STATE}" | grep -Fq 'kueue.x-k8s.io/admission'
printf 'PASS: native preemption is immediate; SIGTERM had only terminationGracePeriodSeconds=%ss, not the business 60s save window\n' "${TERMINATION_GRACE_SECONDS}"
kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  get pods,workloads.kueue.x-k8s.io --output=wide
kubectl --context "${CONTEXT}" --namespace "${NAMESPACE}" \
  get events --sort-by=.lastTimestamp
printf '%s\n' 'POC PASS'
