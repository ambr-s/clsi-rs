#!/bin/bash
# Wrapper that lets wrangler call podman as if it were docker.
# Strips buildx-specific flags podman doesn't accept (--provenance, --sbom, --load).
# When wrangler passes "-f -" (Dockerfile on stdin), spool it to a temp file
# first; podman's stdin handling under wrangler's spawn breaks otherwise.
{
  echo "--- $(date -Iseconds) invoke: $(printf '%q ' "$@")"
} >>/tmp/podman-wrapper.log 2>&1

args=()
needs_dockerfile_spool=0
for a in "$@"; do
  case "$a" in
    --provenance|--provenance=*|--sbom|--sbom=*|--load) ;;
    *) args+=("$a") ;;
  esac
done

# Detect "-f -" pattern in args and replace with a spool file.
new_args=()
spool=""
i=0
while [ $i -lt ${#args[@]} ]; do
  cur="${args[$i]}"
  next="${args[$((i+1))]:-}"
  if [ "$cur" = "-f" ] && [ "$next" = "-" ]; then
    spool=$(mktemp /tmp/dockerfile.XXXXXX)
    cat > "$spool"
    new_args+=("-f" "$spool")
    i=$((i+2))
    continue
  fi
  new_args+=("$cur")
  i=$((i+1))
done

{
  echo "--- final: podman $(printf '%q ' "${new_args[@]}") (spool=$spool)"
} >>/tmp/podman-wrapper.log 2>&1
podman "${new_args[@]}"
ec=$?
[ -n "$spool" ] && rm -f "$spool"
exit $ec
