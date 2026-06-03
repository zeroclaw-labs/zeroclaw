IMAGE_NAME    = zeroclaw
IMAGE_TAG     = stagex

.PHONY: build extract shell-debug clean

build:
	podman build -t $(IMAGE_NAME):$(IMAGE_TAG) -f Containerfile .

extract:
	@if ! podman image exists $(IMAGE_NAME):$(IMAGE_TAG) 2>/dev/null; then \
		$(MAKE) build; \
	fi
	podman create --name zeroclaw-extract $(IMAGE_NAME):$(IMAGE_TAG)
	podman cp zeroclaw-extract:/usr/bin/zeroclaw .
	podman rm zeroclaw-extract
	ls -lh zeroclaw

shell-debug:
	podman run --rm -it \
		--entrypoint /bin/sh \
		docker.io/stagex/pallet-rust@sha256:2d90b9552412ee2c4fa2a13b489c2f28c044be7fb5d6a942bfd5a480a5c288fd

clean:
	-podman rmi $(IMAGE_NAME):$(IMAGE_TAG) 2>/dev/null
	rm -f zeroclaw
