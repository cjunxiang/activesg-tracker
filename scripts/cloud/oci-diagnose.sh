#!/bin/bash
# Why is the micro shape rejecting us? Check shape availability and service
# limits, then replay one launch with the full untruncated error.
set -uo pipefail

T=$(oci iam user list --limit 1 --query 'data[0]."compartment-id"' --raw-output)
AD=$(oci iam availability-domain list --compartment-id "$T" --query 'data[0].name' --raw-output)
SUBNET=$(oci network subnet list --compartment-id "$T" --all \
  --query 'data[?"display-name"==`activesg-subnet`]|[0].id' --raw-output 2>/dev/null)
echo "AD: $AD"; echo "subnet: $SUBNET"; sleep 3

echo
echo "=== shapes offered in this AD ==="
oci compute shape list --compartment-id "$T" --availability-domain "$AD" \
  --query 'data[].shape' --raw-output 2>&1 | head -20
sleep 6

echo
echo "=== A1 limit (default standard-a1-core-count) ==="
oci limits resource-availability get --service-name compute --limit-name standard-a1-core-count \
  --compartment-id "$T" --availability-domain "$AD" \
  --query 'data.{available:available,used:used,limit:limit}' --output table 2>&1 | tail -6
sleep 6

echo
echo "=== E2 micro limit (standard-e2-micro-core-count) ==="
oci limits resource-availability get --service-name compute --limit-name standard-e2-micro-core-count \
  --compartment-id "$T" --availability-domain "$AD" \
  --query 'data.{available:available,used:used,limit:limit}' --output table 2>&1 | tail -6
sleep 6

echo
echo "=== full micro launch error ==="
EMIMG=$(oci compute image list --compartment-id "$T" --operating-system 'Canonical Ubuntu' \
        --operating-system-version '24.04' --shape 'VM.Standard.E2.1.Micro' \
        --sort-by TIMECREATED --sort-order DESC --query 'data[0].id' --raw-output 2>/dev/null)
echo "image: $EMIMG"; sleep 4
oci compute instance launch --compartment-id "$T" --availability-domain "$AD" \
  --shape 'VM.Standard.E2.1.Micro' --image-id "$EMIMG" --subnet-id "$SUBNET" \
  --assign-public-ip true --ssh-authorized-keys-file ~/.ssh/activesg.pub \
  --display-name activesg-collector 2>&1 | head -30
