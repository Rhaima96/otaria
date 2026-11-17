#!/usr/bin/env bash
# Local test servers for development (Docker / OrbStack). Test-only credentials.
#   scripts/test-db.sh up     start MariaDB 12.3 (port 13306) + an SSH bastion (port 12222)
#   scripts/test-db.sh down   remove them
# Rust live test: OTARIA_TEST_DB="127.0.0.1:13306:root:otaria-test" cargo test -- --ignored
set -euo pipefail
NET=otaria-test-net
case "${1:-up}" in
  up)
    docker network inspect $NET >/dev/null 2>&1 || docker network create $NET >/dev/null
    docker run -d --name otaria-test-db --network $NET -p 13306:3306 \
      -e MARIADB_ROOT_PASSWORD=otaria-test mariadb:12.3 >/dev/null
    # SSH bastion: user "tunnel" / password "otaria-ssh", can reach otaria-test-db:3306
    # (reuses the MariaDB image, Ubuntu-based, to avoid a second image pull)
    docker run -d --name otaria-test-ssh --network $NET -p 12222:22 --entrypoint sh mariadb:12.3 -c '
      apt-get update -qq && apt-get install -y -qq openssh-server >/dev/null && mkdir -p /run/sshd &&
      useradd -m tunnel && echo "tunnel:otaria-ssh" | chpasswd &&
      printf "PasswordAuthentication yes\nAllowTcpForwarding yes\n" >> /etc/ssh/sshd_config &&
      exec /usr/sbin/sshd -D -e' >/dev/null
    echo "MariaDB: 127.0.0.1:13306 root/otaria-test · SSH: 127.0.0.1:12222 tunnel/otaria-ssh -> otaria-test-db:3306"
    ;;
  down)
    docker rm -f otaria-test-db otaria-test-ssh >/dev/null 2>&1 || true
    docker network rm $NET >/dev/null 2>&1 || true
    ;;
esac
