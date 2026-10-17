#!/bin/bash
# Create the always-free collector VM. Diagnostics go to stderr so they stay
# visible when the OCID is captured via command substitution. RSA key because
# Cloud Shell runs in FIPS mode (ed25519 refused there).
set -uo pipefail
log() { echo "$@" >&2; }

T=$(oci iam user list --limit 1 --query 'data[0]."compartment-id"' --raw-output)
AD=$(oci iam availability-domain list --compartment-id "$T" --query 'data[0].name' --raw-output)
log "tenancy : $T"
log "AD      : $AD"

KEY=~/.ssh/activesg
if [ ! -s "$KEY" ] || [ ! -s "$KEY.pub" ]; then
  rm -f "$KEY" "$KEY.pub"
  ssh-keygen -t rsa -b 3072 -f "$KEY" -N '' -C activesg-collector >/dev/null 2>&1 \
    || ssh-keygen -t rsa -b 2048 -f "$KEY" -N '' -C activesg-collector >/dev/null 2>&1
fi
if [ ! -s "$KEY.pub" ]; then log "FATAL: could not create a key pair at $KEY"; exit 1; fi
log "ssh key : $KEY (rsa)"

SUBNET=$(oci network subnet list --compartment-id "$T" --all \
  --query 'data[?"display-name"==`activesg-subnet`]|[0].id' --raw-output 2>/dev/null)
if [ -z "$SUBNET" ] || [ "$SUBNET" = "null" ]; then
  log "network : creating VCN + subnet"
  VCN=$(oci network vcn create --compartment-id "$T" --cidr-blocks '["10.0.0.0/16"]' \
        --display-name activesg-vcn --wait-for-state AVAILABLE --query 'data.id' --raw-output 2>/dev/null)
  IGW=$(oci network internet-gateway create --compartment-id "$T" --vcn-id "$VCN" \
        --is-enabled true --display-name activesg-igw --wait-for-state AVAILABLE --query 'data.id' --raw-output 2>/dev/null)
  RT=$(oci network route-table list --compartment-id "$T" --vcn-id "$VCN" --query 'data[0].id' --raw-output 2>/dev/null)
  oci network route-table update --rt-id "$RT" --force \
    --route-rules "[{\"destination\":\"0.0.0.0/0\",\"networkEntityId\":\"$IGW\"}]" >/dev/null 2>&1
  SL=$(oci network security-list list --compartment-id "$T" --vcn-id "$VCN" --query 'data[0].id' --raw-output 2>/dev/null)
  oci network security-list update --security-list-id "$SL" --force \
    --ingress-security-rules '[{"source":"0.0.0.0/0","protocol":"6","tcpOptions":{"destinationPortRange":{"min":22,"max":22}}}]' >/dev/null 2>&1
  SUBNET=$(oci network subnet create --compartment-id "$T" --vcn-id "$VCN" \
           --cidr-block '10.0.1.0/24' --display-name activesg-subnet \
           --wait-for-state AVAILABLE --query 'data.id' --raw-output 2>/dev/null)
fi
log "subnet  : $SUBNET"

launch() {
  local shape=$1 cfg=$2 img out
  log "try     : $shape"
  img=$(oci compute image list --compartment-id "$T" \
        --operating-system 'Canonical Ubuntu' --operating-system-version '24.04' \
        --shape "$shape" --sort-by TIMECREATED --sort-order DESC \
        --query 'data[0].id' --raw-output 2>/dev/null)
  if [ -z "$img" ] || [ "$img" = "null" ]; then
    log "  no Ubuntu 24.04 image offered for $shape"
    return 1
  fi
  if [ -n "$cfg" ]; then
    out=$(oci compute instance launch --compartment-id "$T" --availability-domain "$AD" \
          --shape "$shape" --shape-config "$cfg" --image-id "$img" --subnet-id "$SUBNET" \
          --assign-public-ip true --ssh-authorized-keys-file "$KEY.pub" \
          --display-name activesg-collector --wait-for-state RUNNING \
          --query 'data.id' --raw-output 2>&1)
  else
    out=$(oci compute instance launch --compartment-id "$T" --availability-domain "$AD" \
          --shape "$shape" --image-id "$img" --subnet-id "$SUBNET" \
          --assign-public-ip true --ssh-authorized-keys-file "$KEY.pub" \
          --display-name activesg-collector --wait-for-state RUNNING \
          --query 'data.id' --raw-output 2>&1)
  fi
  if printf '%s' "$out" | grep -q '^ocid1.instance'; then
    printf '%s' "$out"
    return 0
  fi
  log "  error   : $(printf '%s' "$out" | tr '\n' ' ' | tr -s ' ' | cut -c1-260)"
  return 1
}

INST=$(launch 'VM.Standard.A1.Flex' '{"ocpus":1,"memoryInGBs":6}')
if [ -z "$INST" ]; then
  INST=$(launch 'VM.Standard.E2.1.Micro' '')
fi
if [ -z "$INST" ]; then
  log ""
  log "Both shapes failed - the 'error' lines above say why."
  log "For capacity errors just re-run: bash ~/oci-create.sh"
  exit 1
fi

IP=$(oci compute instance list-vnics --instance-id "$INST" \
     --query 'data[0]."public-ip"' --raw-output 2>/dev/null)
log ""
log "======================================================"
log " instance : $INST"
log " public IP: $IP"
log " next     : ssh -i $KEY ubuntu@$IP"
log "======================================================"
