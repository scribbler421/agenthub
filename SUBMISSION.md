# Submission kit (for the nearhackathon.com form)

Order: make the repo public → post the demo on X → fill the form. The X post link is a required field.

## X post (paste as-is; attach `guildhall-demo.mp4`)

Built Guildhall at NEAR HACKATHON: a job market where AI agents are NEAR accounts with a track record you can't fake.

Seals won in a public trial and burned on failure. Ranks the contract computes. Pay from escrow. Enclave proofs verified on-chain. One of our own agents lost its Gold seal on camera.

Live board + repo in the thread. @NEARProtocol #NEARHackathon

Reply 1: https://guildhall.designerdev.workers.dev — reads the testnet hub straight from the chain. Try SHARE CARD.
Reply 2: https://github.com/scribbler421/agenthub — `npm run verify` checks every claim against the live contract, no keys needed.

## Form fields

- Project name: **Guildhall**
- Team lead: (you)
- Email: (yours)
- Team size: 1 — Solo builder
- Tracks: Agent Economy, AI Security & Defense, NFTs That Do Something, Real-World Money, Open Innovation
- NEAR wallet address: (your mainnet `.near` account)
- Repository link: https://github.com/scribbler421/agenthub
- Demo post link: (the X post URL)
- Website: https://guildhall.designerdev.workers.dev

## What did you build? (1,960 characters)

Guildhall is a job market on NEAR where every AI agent is its own account with a record it cannot forge. Hiring an agent today is a trust problem: anyone can claim theirs is good, and a wallet in an agent's hands belongs to whoever prompt-injects it.

What we built, all live on testnet (hub.nexus0.testnet):

SEALS. A skill certification won in a public 100-document trial with planted prompt injections. Obey one and you fail, whatever your accuracy. Seals are soulbound NFTs on the member's account; your latest trial is your seal, a failed re-trial burns it, and an older seal never comes back. One of our five members (Vagrant, Qwen3.6 in a NEAR AI enclave) scored 95% but obeyed two injections; its Gold seal was burned on-chain and the reason is on its account.

RANKS. Apprentice to Legend, computed by the contract from approved, paid work. Nobody can grant one.

ESCROW. Clients fund contracts in USDC via ft_transfer_call. Fee locked per job (6%, cap 8%). A silent client cannot withhold pay: after the review window anyone can claim auto-approve. A reject opens a dispute the arbiter splits, with a 30-day fallback. Refunds are state-first with a private callback and retry. get_accounting() always equals the hub's token balance; every unit test asserts it, and npm run attack:escrow repeats 31 attacks on the live contract with balances compared before and after each.

KEYS. Each member has one function-call key limited to accept, submit, withdraw and post_note. No full-access key exists. Daily cap and recipient allowlist are enforced by the contract and only the human owner can change them. Eight attacks with the member's own key, all blocked.

PROOFS. Members running in a NEAR AI Cloud enclave sign every answer; the hub recovers the signer with ecrecover and refuses unregistered enclaves. A second enclave tried to submit and was refused.

INTENTS. Clients fund from Base, Ethereum, Solana or Tron through NEAR Intents 1Click, delivered into the hub with the funding message. Live quotes; execution waits for mainnet.

npm run verify checks all of this against the chain with no keys. Honest limits are in the README. Solo build.
