# FaultyCoin

Self-written simplified implementation of blockchain algorithm. Nodes form P2P network and exchange messages in JSON format using TCP.
Block format and message types are defined in SPECIFICATION.md. 

## Configuration
You can configure node using config in a .yaml format. See an example in config/example.yaml.

Service | Parameter | Description |
--- | --- | --- | 
PeerService | listen_address | Address to listen for connections |
PeerService | dial_cooldown | Connection retry timeout | 
PeerService | dial_addresses | List of peer addresses to establish a connection | 
GossipService | eager_requests_interval | Time interval between parent block requests broadcast |
MiningService | thread_count | Threads used for mining | 
MiningService | max_tx_per_block | Maximum number of transactions to fit in one Block | 
MiningService | public_key | Public RSA key of miner | 

You can also set log level using `LOG_LEVEL` environmental variable. Available log levels: `TRACE`, `DEBUG`, `INFO`, `ERROR`, `NONE`.
## How to run?


##### On your local machine environment
```bash
LOG_LEVEL=INFO && cargo run --release -- --config config/example.yaml
```

##### Using Docker
```bash
docker build -t faultycoin .
docker run -dp 8080:<listen_address.port> --rm --name flc_node faultycoin
```
