#!/usr/bin/env bash

set -euo pipefail
export LC_ALL=C

allow_downgrade=false
if [[ "${1:-}" == "--allow-downgrade" ]]; then
  allow_downgrade=true
  shift
fi

if (( $# != 4 )); then
  echo "usage: $0 [--allow-downgrade] <target-srcinfo> <current-srcinfo> <target-pkgbuild> <current-pkgbuild>" >&2
  exit 2
fi

target_srcinfo="$1"
current_srcinfo="$2"
target_pkgbuild="$3"
current_pkgbuild="$4"
dotted_pattern='^[0-9]+(\.[0-9]+)*$'
integer_pattern='^[0-9]+$'

for package_file in "$target_srcinfo" "$target_pkgbuild"; do
  if [[ ! -f "$package_file" ]]; then
    echo "::error::Required generated AUR package file does not exist: ${package_file}" >&2
    exit 2
  fi
done

read_srcinfo_field() {
  local file="$1"
  local field="$2"
  local pattern="$3"
  local default_value="$4"
  local label="$5"
  local value
  local -a values=()

  while IFS= read -r value; do
    values+=("$value")
  done < <(
    awk -v wanted="$field" '
      {
        separator = index($0, "=")
        if (separator == 0) {
          next
        }
        key = substr($0, 1, separator - 1)
        sub(/^[[:space:]]+/, "", key)
        sub(/[[:space:]]+$/, "", key)
        if (key != wanted) {
          next
        }
        value = substr($0, separator + 1)
        sub(/^[[:space:]]+/, "", value)
        sub(/[[:space:]]+$/, "", value)
        print value
      }
    ' "$file"
  )

  if (( ${#values[@]} == 0 )) && [[ -n "$default_value" ]]; then
    printf '%s' "$default_value"
    return 0
  fi
  if (( ${#values[@]} != 1 )); then
    echo "::error::Expected exactly one ${field} in ${label} .SRCINFO; found ${#values[@]}." >&2
    return 2
  fi
  if [[ ! "${values[0]}" =~ $pattern ]]; then
    echo "::error::${label} AUR ${field} is not numeric: ${values[0]}" >&2
    return 2
  fi

  printf '%s' "${values[0]}"
}

read_tuple() {
  local file="$1"
  local label="$2"
  local epoch pkgver pkgrel

  epoch="$(read_srcinfo_field "$file" epoch "$integer_pattern" 0 "$label")" || return 2
  pkgver="$(read_srcinfo_field "$file" pkgver "$dotted_pattern" "" "$label")" || return 2
  pkgrel="$(read_srcinfo_field "$file" pkgrel "$dotted_pattern" "" "$label")" || return 2
  printf '%s\t%s\t%s' "$epoch" "$pkgver" "$pkgrel"
}

read_pkgbuild_field() {
  local file="$1"
  local field="$2"
  local pattern="$3"
  local default_value="$4"
  local label="$5"
  local value
  local -a values=()

  while IFS= read -r value; do
    values+=("$value")
  done < <(
    awk -v wanted="$field" '
      {
        separator = index($0, "=")
        if (separator == 0) {
          next
        }
        key = substr($0, 1, separator - 1)
        sub(/^[[:space:]]+/, "", key)
        sub(/[[:space:]]+$/, "", key)
        if (key != wanted) {
          next
        }
        value = substr($0, separator + 1)
        sub(/^[[:space:]]+/, "", value)
        sub(/[[:space:]]+$/, "", value)
        if (value ~ /^\".*\"$/ || value ~ /^\047.*\047$/) {
          value = substr(value, 2, length(value) - 2)
        }
        print value
      }
    ' "$file"
  )

  if (( ${#values[@]} == 0 )) && [[ -n "$default_value" ]]; then
    printf '%s' "$default_value"
    return 0
  fi
  if (( ${#values[@]} != 1 )); then
    echo "::error::Expected exactly one ${field} in ${label} PKGBUILD; found ${#values[@]}." >&2
    return 2
  fi
  if [[ ! "${values[0]}" =~ $pattern ]]; then
    echo "::error::${label} PKGBUILD ${field} is not numeric: ${values[0]}" >&2
    return 2
  fi

  printf '%s' "${values[0]}"
}

read_pkgbuild_tuple() {
  local file="$1"
  local label="$2"
  local epoch pkgver pkgrel

  epoch="$(read_pkgbuild_field "$file" epoch "$integer_pattern" 0 "$label")" || return 2
  pkgver="$(read_pkgbuild_field "$file" pkgver "$dotted_pattern" "" "$label")" || return 2
  pkgrel="$(read_pkgbuild_field "$file" pkgrel "$dotted_pattern" "" "$label")" || return 2
  printf '%s\t%s\t%s' "$epoch" "$pkgver" "$pkgrel"
}

normalize_component() {
  local component="$1"
  component="${component#"${component%%[!0]*}"}"
  printf '%s' "${component:-0}"
}

compare_numeric_dotted() {
  local left="$1"
  local right="$2"
  local index left_component right_component max_components
  local -a left_parts right_parts

  IFS='.' read -r -a left_parts <<< "$left"
  IFS='.' read -r -a right_parts <<< "$right"
  max_components=${#left_parts[@]}
  if (( ${#right_parts[@]} > max_components )); then
    max_components=${#right_parts[@]}
  fi

  for (( index = 0; index < max_components; index++ )); do
    left_component="$(normalize_component "${left_parts[index]:-0}")"
    right_component="$(normalize_component "${right_parts[index]:-0}")"

    if (( ${#left_component} < ${#right_component} )); then
      printf '%s' -1
      return 0
    fi
    if (( ${#left_component} > ${#right_component} )); then
      printf '%s' 1
      return 0
    fi
    if [[ "$left_component" < "$right_component" ]]; then
      printf '%s' -1
      return 0
    fi
    if [[ "$left_component" > "$right_component" ]]; then
      printf '%s' 1
      return 0
    fi
  done

  printf '%s' 0
}

target_tuple="$(read_tuple "$target_srcinfo" Target)" || exit 2
target_pkgbuild_tuple="$(read_pkgbuild_tuple "$target_pkgbuild" Target)" || exit 2
if [[ "$target_tuple" != "$target_pkgbuild_tuple" ]]; then
  echo "::error::Generated AUR .SRCINFO and PKGBUILD disagree on epoch, pkgver, or pkgrel." >&2
  exit 2
fi

if [[ ! -e "$current_srcinfo" && ! -e "$current_pkgbuild" ]]; then
  echo "::error::The cloned AUR repository is unexpectedly empty; refusing an implicit first publish." >&2
  exit 2
fi
if [[ ! -f "$current_srcinfo" || ! -f "$current_pkgbuild" ]]; then
  echo "::error::The cloned AUR repository is partially populated; both .SRCINFO and PKGBUILD are required." >&2
  exit 2
fi

current_tuple="$(read_tuple "$current_srcinfo" Current)" || exit 2
current_pkgbuild_tuple="$(read_pkgbuild_tuple "$current_pkgbuild" Current)" || exit 2
if [[ "$current_tuple" != "$current_pkgbuild_tuple" ]]; then
  echo "::error::Cloned AUR .SRCINFO and PKGBUILD disagree on epoch, pkgver, or pkgrel." >&2
  exit 2
fi
IFS=$'\t' read -r target_epoch target_pkgver target_pkgrel <<< "$target_tuple"
IFS=$'\t' read -r current_epoch current_pkgver current_pkgrel <<< "$current_tuple"

tuple_order=0
for pair in \
  "$target_epoch $current_epoch" \
  "$target_pkgver $current_pkgver" \
  "$target_pkgrel $current_pkgrel"; do
  read -r target_component current_component <<< "$pair"
  tuple_order="$(compare_numeric_dotted "$target_component" "$current_component")"
  if [[ "$tuple_order" != 0 ]]; then
    break
  fi
done

target_display="${target_epoch}:${target_pkgver}-${target_pkgrel}"
current_display="${current_epoch}:${current_pkgver}-${current_pkgrel}"

if [[ "$tuple_order" == -1 ]]; then
  if [[ "$allow_downgrade" == true ]]; then
    if [[ "$(compare_numeric_dotted "$target_epoch" "$current_epoch")" != 0 ]]; then
      echo "::error::Refusing manual AUR downgrade override across an epoch boundary: target ${target_display}, current ${current_display}. Preserve the published epoch and cut a new release tag." >&2
      exit 3
    fi
    echo "::warning::Manual AUR downgrade override accepted target ${target_display} over current ${current_display}." >&2
    exit 0
  fi
  echo "::error::Refusing AUR downgrade: target ${target_display} is older than current ${current_display}." >&2
  exit 3
fi

if [[ "$tuple_order" == 0 ]]; then
  if ! cmp -s "$target_srcinfo" "$current_srcinfo" || \
    ! cmp -s "$target_pkgbuild" "$current_pkgbuild"; then
    echo "::error::Target and current AUR package files differ at the same version ${target_display}; a changed package must ship under a new version tuple from a new release tag." >&2
    exit 4
  fi
  echo "AUR version guard accepted the unchanged package at ${target_display}."
  exit 0
fi

echo "AUR version guard accepted target ${target_display} (current ${current_display})."
