IMAGE_NAME    = zeroclaw
IMAGE_TAG     = stagex
IMAGE_FAT_TAG = stagex-fat

.PHONY: build build-fat extract extract-fat shell-debug clean

build:
	podman build -t $(IMAGE_NAME):$(IMAGE_TAG) --target package -f Containerfile .

build-fat:
	podman build -t $(IMAGE_NAME):$(IMAGE_FAT_TAG) --target package-fat -f Containerfile .

extract:
	@if ! podman image exists $(IMAGE_NAME):$(IMAGE_TAG) 2>/dev/null; then \
		$(MAKE) build; \
	fi
	podman create --name zeroclaw-extract $(IMAGE_NAME):$(IMAGE_TAG)
	podman cp zeroclaw-extract:/usr/bin/zeroclaw .
	podman cp zeroclaw-extract:/usr/bin/zerocode .
	podman rm zeroclaw-extract
	ls -lh zeroclaw zerocode

extract-fat:
	@if ! podman image exists $(IMAGE_NAME):$(IMAGE_FAT_TAG) 2>/dev/null; then \
		$(MAKE) build-fat; \
	fi
	podman create --name zeroclaw-fat-extract $(IMAGE_NAME):$(IMAGE_FAT_TAG)
	podman cp zeroclaw-fat-extract:/usr/bin/zeroclaw .
	podman cp zeroclaw-fat-extract:/usr/bin/zerocode .
	podman rm zeroclaw-fat-extract
	mv zeroclaw zeroclaw-fat
	ls -lh zeroclaw-fat

shell-debug:
	podman run --rm -it \
		--entrypoint /bin/sh \
		docker.io/stagex/pallet-rust@sha256:abe9b95c93a5afa271f69fcd5eb18c8cd405fe5df6491a63c9418e3a170573dc

clean:
	-podman rmi $(IMAGE_NAME):$(IMAGE_TAG) $(IMAGE_NAME):$(IMAGE_FAT_TAG) 2>/dev/null
	rm -f zeroclaw zerocode zeroclaw-fat
