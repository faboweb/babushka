# Create contract
# source <(curl -sSL https://raw.githubusercontent.com/CosmWasm/testnets/master/malaga-420/defaults.env)
cd ../..
make install
RES=$(neutrond tx wasm store artifacts/babu-aarch64.wasm --from babu $NODE -y --output json -b block --gas-prices 0.025untrn --gas-adjustment 1.7 --gas auto --chain-id pion-1)
# CODE_ID=$(echo $RES | jq -r '.logs[0].events[-1].attributes[0].value')
neutrond query wasm list-code $NODE --page 17
CODE_ID=...
INIT='{}'
neutrond tx wasm instantiate $CODE_ID "$INIT" --from babu --label "babu test" -y --admin "neutron14jkyrmk8n0hsdqqr7vg5clhasxpt5ajd0e6zm9" --gas-prices 0.025untrn --gas-adjustment 1.7 --gas auto $NODE
CONTRACT=$(neutrond query wasm list-contract-by-code $CODE_ID $NODE --output json | jq -r '.contracts[-1]')
echo $CONTRACT

# neutron18guzurkjk9sq65xkv04yd7a5mj90u7qx36w36w09akx8zdhdagaqmvnnkx

# neutron16fc5zd8czxh688mrsn60wzju44czuk3f4mvkmv

# Update contract
source <(curl -sSL https://raw.githubusercontent.com/CosmWasm/testnets/master/malaga-420/defaults.env)
cargo wasm
# cargo schema
docker run --rm -v "$(pwd)":/code \
  --mount type=volume,source="$(basename "$(pwd)")_cache",target=/code/target \
  --mount type=volume,source=registry_cache,target=/usr/local/cargo/registry \
  cosmwasm/rust-optimizer-arm64:0.12.13
RES=$(neutrond tx wasm store artifacts/babu-aarch64.wasm --from babu $TXFLAG -y)
# CODE_ID=$(echo $RES | jq -r '.logs[0].events[-1].attributes[1].value')
neutrond query wasm list-code $NODE --page 17
CODE_ID=...
CONTRACT='neutron18guzurkjk9sq65xkv04yd7a5mj90u7qx36w36w09akx8zdhdagaqmvnnkx'
ARGS='{}'
neutrond tx wasm migrate $CONTRACT $CODE_ID "$ARGS" --from babu $TXFLAG -y

neutrond tx wasm execute neutron18guzurkjk9sq65xkv04yd7a5mj90u7qx36w36w09akx8zdhdagaqmvnnkx "{\"register\":{\"connection_id\":\"connection-10\", \"interchain_account_id\":\"babu_neutron_osmosis_v1\"}}" --from babu $TXFLAG
neutrond tx wasm execute neutron18guzurkjk9sq65xkv04yd7a5mj90u7qx36w36w09akx8zdhdagaqmvnnkx "{\"fund\":{}}" --amount 1untrn --from babu $TXFLAG 