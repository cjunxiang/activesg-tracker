#!/bin/bash
# A1 capacity, small-footprint sweep. Free allowance is 1500 OCPU-hours and
# 9000 GB-hours/month (= ~2 OCPU / 12 GB continuous), so 1 OCPU with 1-6 GB is
# comfortably inside it. Smaller memory fits host bins that 6/12 GB cannot.
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
log "AD    : $AD"
log "FDs   : $(echo "$FDS" | tr '\n' ' ')"
log "image : ${IMG:0:28}..."
sleep 4

# existing instance short-circuit
EX=$(oci compute instance list --compartment-id "$T" --all \
     --query 'data[?"display-name"==`activesg-collector`]|[0].id' --raw-output 2>/dev/null)
if [ -n "$EX" ] && [ "$EX" != "null" ]; then
  log "already exists: $EX"; exit 0
fi

OK=""
try() {
  local fd=$1 ocpu=$2 mem=$3 out
  log "  FD=$fd  ${ocpu}ocpu/${mem}gb"
  out=$(oci compute instance launch --compartment-id "$T" --availability-domain "$AD" \
        --fault-domain "$fd" --shape 'VM.Standard.A1.Flex' \
        --shape-config "{\"ocpus\":$ocpu,\"memoryInGBs\":$mem}" \
        --image-id "$IMG" --subnet-id "$SUBNET" --assign-public-ip true \
        --ssh-authorized-keys-file ~/.ssh/activesg.pub \
        --display-name activesg-collector --wait-for-state RUNNING \
        --query 'data.id' --raw-output 2>&1)
  if printf '%s' "$out" | grep -q '^ocid1.instance'; then
    log "  ==> SUCCESS ${ocpu}ocpu/${mem}gb in $fd"
    OK=$out
    return 0
  fi
  case "$out" in
    *TooManyRequests*) log "      rate limited -> 90s"; sleep 90; return 2 ;;
    *"Out of host capacity"*) log "      no capacity"; sleep 20; return 3 ;;
    *) log "      $(printf '%s' "$out" | grep -oE '"message": "[^"]*"' | head -1)"; sleep 20; return 1 ;;
  esac
}

for mem in 1 2 3 4 6; do
  for fd in $(echo "$FDS"); do
    try "$fd" 1 "$mem"
    [ -n "$OK" ] && break 2
  done
done

if [ -z "$OK" ]; then
  log ""
  log "Still no capacity at 1 OCPU with 1-6 GB across all fault domains."
  log "Same free allowance is unaffected - this is Oracle's host pool, not you."
  log "Two remaining moves:"
  log "  1. wait and re-run:  bash ~/oci-small.sh"
  log "  2. upgrade to Pay As You Go (Always Free resources stay free, and paid"
  log "     accounts get priority on capacity): Console > Billing > Upgrade"
  exit 1
fi

IP=$(oci compute instance list-vnics --instance-id "$OK" \
     --query 'data[0]."public-ip"' --raw-output 2>/dev/null)
log ""
log "======================================================"
log " instance : $OK"
log " public IP: $IP"
log " next     : ssh -i ~/.ssh/activesg ubuntu@$IP"
log "======================================================"
