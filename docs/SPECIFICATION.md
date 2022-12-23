## Block description

```
{
    "index": 1
    "nonce": 33333,
    "reward": 10
    "issuer": "<RSA_KEY>",
    "timestamp": <UNIX_TIMESTAMP>,
    "max_hash": "<SHA3_512_HASH>",
    "prev_hash": "<SHA3_512_HASH>",
    "transactions": [
        {
            "amount": 500
            "fee": 30
            "comment": "Hello, world",
            "sender": "<RSA_KEY>",
            "receiver": "<RSA_KEY>",
            "signature": ""
        }
    ]
}

```

Public RSA keys and hashes are encoded in Base64 format. 

## Messages description

There are 3 type of messages:
1. "Block" - the sender informs the recipient about the presence of a valid block from the sender's point of view.
```
{
    "kind": "block",
    // all block attributes 
}
```
2. "Transaction" - The sender informs the receiver that, from the sender's point of view, there is some valid transaction that is not yet present in any block.

```
{
    "kind": "transaction",
    // all transaction attributes 
}
```

3. "Block Request" - The sender informs the recipient that he wants to receive a block whose hash is equal to the specified hash.
```
{
    "kind": "request",
    "block_hash": "<SHA3_512_HASH>"
}
```