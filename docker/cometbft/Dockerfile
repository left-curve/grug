FROM golang:1.25-bookworm AS builder

LABEL org.opencontainers.image.description="CometBFT builder for Dango"
LABEL org.opencontainers.image.source="https://github.com/left-curve/left-curve"

# Install dependencies.
RUN apt update && apt install -y git make

# Git tag of the cometbft repo.
ARG COMETBFT_GIT_TAG="v0.39.3"
ENV COMETBFT_GIT_TAG=$COMETBFT_GIT_TAG

# Download cometbft source code, build, then delete.
RUN git clone --branch $COMETBFT_GIT_TAG --depth 1 https://github.com/cometbft/cometbft.git \
  && cd cometbft \
  && make install

FROM debian:bookworm-slim AS runtime

LABEL org.opencontainers.image.description="CometBFT runtime for Dango"

# Install dependencies.
RUN apt update && apt upgrade -y && apt install -y curl

# I changed my mind, adding a user with a specific UID/GID (in this case
# 1001/1001) is a pain because the files created from within that container
# will belong to that UID/GID, which may be different from the host current
# user.
# Instead we'll run as root, like official Debian images, and it's up to whoever
# is using this docker image to use the `--user` docker feature to not run as
# root.
# ARG USERNAME=cometbft
# ENV USERNAME=${USERNAME}
# RUN useradd --create-home --shell /bin/bash $USERNAME

COPY --from=builder /go/bin/cometbft /usr/local/bin/
# RUN chown $USERNAME:$USERNAME /usr/local/bin/cometbft

# USER $USERNAME
# WORKDIR /home/$USERNAME
# WORKDIR /app

# Expose the Tendermint p2p (26656), RPC (26657), ABCI (26658) and metrics (26660) ports.
EXPOSE 26656 26657 26658 26660

# Run cometbft.
# This assume the config folder has been mounted to `/root/.cometbft/config`.
CMD ["cometbft", "start"]
