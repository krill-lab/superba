#!/usr/bin/env bash
# Install Ubuntu packages on a GitHub runner: `apt-install.sh <package>...`.
#
# The runner image points apt at azure.archive.ubuntu.com (and
# archive/security.ubuntu.com). When those names do not resolve, which has
# happened for hours at a time, every Linux job failed at this step.
# Then this script points apt at public mirrors on other domains and tries again.
set -euo pipefail

[ "$#" -gt 0 ] || { echo "usage: $0 <package>..." >&2; exit 2; }

attempt() {
  sudo apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30 update &&
    sudo apt-get -o Acquire::Retries=3 -o Acquire::http::Timeout=30 install -y "$@"
}

if attempt "$@"; then
  exit 0
fi
echo "::warning::apt failed with the runner's Ubuntu mirrors; trying public mirrors" >&2

. /etc/os-release
codename="${VERSION_CODENAME:?}"
if [ "$(dpkg --print-architecture)" = amd64 ]; then
  mirrors="http://mirrors.edge.kernel.org/ubuntu https://mirror.leaseweb.net/ubuntu http://mirrors.mit.edu/ubuntu"
else
  mirrors="http://mirrors.edge.kernel.org/ubuntu-ports https://mirror.leaseweb.net/ubuntu-ports http://ports.ubuntu.com/ubuntu-ports"
fi

# Take the image's own source lists out of the way (deb822 and one-line forms).
sudo mkdir -p /etc/apt/disabled-sources
for f in /etc/apt/sources.list /etc/apt/sources.list.d/*.list /etc/apt/sources.list.d/*.sources; do
  if [ -e "$f" ]; then
    sudo mv "$f" /etc/apt/disabled-sources/
  fi
done

for m in $mirrors; do
  host="${m#*://}"
  host="${host%%/*}"
  if ! getent hosts "$host" >/dev/null; then
    echo "skip $m: $host does not resolve" >&2
    continue
  fi
  echo "trying $m" >&2
  printf 'deb %s %s main universe\ndeb %s %s-updates main universe\ndeb %s %s-security main universe\n' \
    "$m" "$codename" "$m" "$codename" "$m" "$codename" | sudo tee /etc/apt/sources.list >/dev/null
  if attempt "$@"; then
    exit 0
  fi
done
echo "::error::apt could not install: $*" >&2
exit 1
