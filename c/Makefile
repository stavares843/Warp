
clean: 
				rm -rf bin

build_static_tesseract:
				mkdir -p bin && clang -Wall -Wextra tesseract.c -o bin/tesseract ../target/debug/libwarp.a -lm

build_static_crypto:
				mkdir -p bin && clang -Wall -Wextra crypto.c -o bin/crypto ../target/debug/libwarp.a -lm

build_static_fs_memory:
				mkdir -p bin && clang -Wall -Wextra fs_memory.c -o bin/fs_memory ../target/debug/libwarp.a ../target/debug/libwarp_fs_memory.a -lm

build_static_fs_ipfs:
				mkdir -p bin && clang -Wall -Wextra fs_ipfs.c -o bin/fs_ipfs ../target/debug/libwarp.a ../target/debug/libwarp_fs_ipfs.a ../target/debug/libwarp_mp_ipfs.a -lm -lz

build_shared_fs_ipfs:
				mkdir -p bin && clang -Wall -Wextra fs_ipfs.c -o bin/fs_ipfs -L ../target/debug -lwarp -lwarp_mp_ipfs -lwarp_fs_ipfs -lm -lz

build_static_fs_storj:
				mkdir -p bin && clang -Wall -Wextra fs_storj.c -o bin/fs_storj ../target/debug/libwarp.a ../target/debug/libwarp_fs_storj.a ../target/debug/libwarp_pd_flatfile.a -lm -lssl -lcrypto

build_shared_tesseract:
				mkdir -p bin && clang -Wall -Wextra tesseract.c -o bin/tesseract -L ../target/debug -lwarp -lpthread -ldl -lssl -lcrypto -lbz2 -lm -ludev

build_shared_mp_ipfs:
				mkdir -p bin && clang -Wall -Wextra mp_ipfs.c -o bin/mp_ipfs -L ../target/debug -lwarp -lwarp_mp_ipfs -lz -lm

build_static_mp_ipfs:
				mkdir -p bin && clang -Wall -Wextra mp_ipfs.c -o bin/mp_ipfs ../target/debug/libwarp.a ../target/debug/libwarp_mp_ipfs.a -lz -lm

build_static_pd_flatfile:
				mkdir -p bin && clang -Wall -Wextra pd_flatfile.c -o bin/pd_flatfile ../target/debug/libwarp.a ../target/debug/libwarp_pd_flatfile.a -lm

build_shared_pd_flatfile:
				mkdir -p bin && clang -Wall -Wextra pd_flatfile.c -o bin/pd_flatfile -L ../target/debug -lwarp -lwarp_pd_flatfile -lpthread -ldl -lm

build_static_rg_ipfs:
				mkdir -p bin && clang -Wall -Wextra rg_ipfs.c -o bin/rg_ipfs ../target/debug/libwarp.a ../target/debug/libwarp_rg_ipfs.a ../target/debug/libwarp_mp_ipfs.a -lz -lm

build_shared_rg_ipfs:
				mkdir -p bin && clang -Wall -Wextra rg_ipfs.c -o bin/rg_ipfs -L ../target/debug -lwarp -lwarp_mp_ipfs -lwarp_rg_ipfs -lz -lm