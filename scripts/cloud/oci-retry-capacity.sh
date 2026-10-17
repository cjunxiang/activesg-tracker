#!/bin/bash
# A1 capacity is per fault domain, so sweep FD-1..3 with two shape sizes.
# One launch call per combo, backed off on Oracle's per-user rate limit.
set -uo pipefail
log() { echo "$@" >&2; }

T=$(oci iam user list --limit 1 --query 'data[0]."compartment-id"' --raw-output)
AD=$(oci iam availability-domain list --compartment-id "$T" --query 'data[0].name' --raw-output)
SUBNET=$(oci network subnet list --compartment-id "$T" --all \
  --query 'data[?"display-name"==`activesg-subnet`]|[0].id' --raw-output 2>/dev/null)
IMG=$(oci compute image list --compartment-id "$T" --operating-system 'Canonical Ubuntu' \
      --operating-system-version '24.04' --shape 'VM.Standard.A1.Flex' \
      --sort-by TIMECREATED --sort-order DESC --query 'data[0].id' --raw-output 2>/dev/null)
sleep 4
FDS=$(oci iam fault-domain list --compartment-id "$T" --availability-domain "$AD" \
      --query 'data[].name' --raw-output 2>/dev/null | tr -d '[]", ' | grep -v '^$')
log "AD      : $AD"
log "image   : ${IMG:0:30}..."
log "FDs     : $(echo "$FDS" | tr '\n' ' ')"
sleep 4

ok=""
try() {
  local fd=$1 cfg=$2 out
  log "  try FD=${fd:-auto} cfg=$cfg"
  out=$(if [ -n "$fd" ]; then
          oci compute instance launch --compartment-id "$T" --availability-domain "$AD" \
            --fault-domain "$fd" --shape 'VM.Standard.A1.Flex' --shape-config "$cfg" \
            --image-id "$IMG" --subnet-id "$SUBNET" --assign-public-ip true \
            --ssh-authorized-keys-file ~/.ssh/activesg.pub \
            --display-name activesg-collector --wait-for-state RUNNING \
            --query 'data.id' --raw-output 2>&1
        else
          oci compute instance launch --compartment-id "$T" --availability-domain "$AD" \
            --shape 'VM.Standard.A1.Flex' --shape-config "$cfg" \
            --image-id "$IMG" --subnet-id "$SUBNET" --assign-public-ip true \
            --ssh-authorized-keys-file ~/.ssh/activesg.pub \
            --display-name activesg-collector --wait-for-state RUNNING \
            --query 'data.id' --raw-output 2>&1
        fi)
  if printf '%s' "$out" | grep -q '^ocid1.instance'; then
    log "  SUCCESS"
    ok=$out
    return 0
  fi
  case "$out" in
    *TooManyRequests*) log "    rate limited -> 90s"; sleep 90 ;;
    *"Out of host capacity"*) log "    no capacity here" ; sleep 8 ;;
    *) log "    $(printf '%s' "$out" | grep -oE '"message": "[^"]*"' | head -1)"; sleep 8 ;;
  esac
  return 1
}

for cfg in '{"ocpus":1,"memoryInGBs":6}' '{"ocpus":2,"memoryInGBs":12}'; do
  for fd in $(echo "$FDS") ""; do
    try "$fd" "$cfg" && break 2
  done
  log "  (moving to next shape size)"
done

if [ -z "$ok" ]; then
  log ""
  log "No A1 capacity in any fault domain right now."
  log "Re-run later: bash ~/oci-capacity.sh   (capacity frees up throughout the day)"
  exit 1
fi

IP=$(oci compute instance list-vnics --instance-id "$ok" \
     --query 'data[0]."public-ip"' --raw-output 2>/dev/null)
log ""
log "======================================================"
log " instance : $ok"
log " public IP: $IP"
log " next     : ssh -i ~/.ssh/activesg ubuntu@$IP"
log "======================================================"
