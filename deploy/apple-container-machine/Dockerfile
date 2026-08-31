# Apple container machine image for a Treer Host.
# Build on macOS 26+ Apple silicon:
#   container build -t local/treer-machine:latest \
#     -f deploy/apple-container-machine/Dockerfile \
#     deploy/apple-container-machine
#
# Requires /sbin/init (systemd). Do not bake Treer binaries into this image;
# install them from the Proxy install.sh after the machine boots.
FROM ubuntu:24.04

ENV container=container \
    DEBIAN_FRONTEND=noninteractive

RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        dbus \
        git \
        iproute2 \
        iptables \
        python3 \
        sudo \
        systemd \
        systemd-sysv \
        vim-tiny \
        wget && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/*

RUN : >/etc/machine-id && : >/var/lib/dbus/machine-id

RUN systemctl set-default multi-user.target && \
    systemctl mask \
        dev-hugepages.mount \
        sys-fs-fuse-connections.mount \
        systemd-update-utmp.service \
        systemd-tmpfiles-setup.service \
        console-getty.service && \
    systemctl disable networkd-dispatcher.service || true

CMD ["/sbin/init"]
