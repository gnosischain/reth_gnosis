git clone https://github.com/gnosischain/reth_gnosis.git
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

sudo apt update
sudo apt install pipx

pipx ensurepath
pipx install gdown
pipx ensurepath

cargo install jwt-cli

sudo apt install build-essential
sudo apt install manpages-dev
sudo apt install build-essential libc6-dev clang libclang-dev pkg-config
sudo apt install libssl-dev libclang-dev
sudo apt install supervisor

cd reth_gnosis/
cargo update
cargo build

gdown https://drive.google.com/file/d/1QLOrLaMe-_CI9OJEjIvoCua3ocvl1bCH/view?usp=sharing --fuzzy
gdown https://drive.google.com/file/d/1OAQULMXt0QN46sEB3IV9KhKHi523Fexq/view?usp=drive_link --fuzzy

# sudo nano /etc/supervisor/conf.d/gnosis-import.conf
# [program:gnosis-import]
# command=bash -c "cd /root/reth_gnosis && ./target/debug/reth --chain ./scripts/mainnet_post_merge.json init-state ../state_at_26478650.jsonl --without-evm --header ../block_26478650_selfencoded.rlp --total-difficulty 8626000110427540000000000000000000000000000000 --header-hash a133198478cb01b4585604d07f584633f1f147103b49672d2bd87a5a3ba2c06e"
# autostart=false
# autorestart=false
# stderr_logfile=/var/log/gnosis-import.err.log
# stdout_logfile=/var/log/gnosis-import.out.log
# logfile_maxbytes=16MB ; (max main logfile bytes b4 rotation;default 50MB)

sudo supervisorctl reread && sudo supervisorctl update
