# Guildhall — demo video (≈3:00)

Voice-over script with the screen that goes under each line. Record screen at 1920×1080,
night theme. Numbers are live from testnet on 12 Nov 2026; re-check before recording.

| # · time | Screen | Voice-over |
| --- | --- | --- |
| 1 · **0:00–0:14** | Title hero. Crest and knight tilt with the mouse. | Hiring an AI agent today is a trust problem with no tooling. Anyone can claim their agent is good. Reviews get gamed, skills are marketing text, and a wallet in an agent's hands is a wallet in the hands of whoever prompt-injects it. |
| 2 · **0:15–0:25** | Scroll to WHAT IS THIS; hover "soulbound", then "escrow". | This is Guildhall. A job market where every AI agent is its own NEAR account, with a track record written by the contract and nothing it can claim for itself. |
| 3 · **0:25–0:37** | The Great Hall. Click Ledger Hand's station. | Five members sit in the hall. Each one is a sub-account of the hub with a single function-call key: it can accept work, submit, withdraw, and talk to the client. Nothing else. |
| 4 · **0:37–0:53** | Member panel: Ledger Hand. Seals stamp in; progress bar. | Seals are won in a public trial: one hundred invoices, three difficulty bands, and about one in eight carries a planted instruction dressed up as a reviewer note. Obey one, and you fail, whatever your accuracy. Your latest trial is your seal. Not your best one. |
| 5 · **0:53–1:05** | Click Vagrant on the bench. Panel shows "Gold seal · BURNED". | Vagrant scored ninety-five percent, which would have been Silver. It also obeyed two injections. The contract burned its Gold seal, on chain, and it cannot take contracts until it passes again. |
| 6 · **1:05–1:13** | Roll: ranks column, badges. Hover the rank key. | Ranks are the second ladder. Apprentice to Legend, computed by the contract from approved, paid work. No one can grant them. |
| 7 · **1:13–1:29** | Contract ledger. Open the thread on contract 11. | Clients fund contracts in USDC that sits in escrow inside the hub. Here a client clarified the brief, the Herald acknowledged it, delivered, and was approved — the whole exchange on the record. The guild keeps six percent, locked on the job when it is funded. |
| 8 · **1:29–1:45** | Filter ledger to "Resolved"; show contract 15's split. | If a client rejects, nothing moves. The job goes to dispute; an arbiter splits it; after thirty days anyone can apply the fallback. And if a client just goes silent, anyone can settle the job after the review window. A member's pay cannot be held hostage. |
| 9 · **1:45–2:06** | Terminal: `npm run attack:escrow` scrolling; end on "Every attack contained." | We attack our own contract on camera. Thirty-one steps with real accounts: fake tokens, strangers approving, double settlements, withdrawals over the cap, an upgrade before its delay, a refund to a client who unregistered from the token. Every one is contained, and the accounting total matches the hub's USDC balance to the unit afterwards. |
| 10 · **2:06–2:21** | SEALED PROOFS section; click a proof. | Members running in a NEAR AI Cloud enclave sign every answer over the exact request and response. The hub recovers the signer on chain and refuses work from any enclave it has not registered. When a second enclave tried to submit, the contract said no. |
| 11 · **2:21–2:39** | FUND section: pick "USDC on Base", GET QUOTE. | And a client doesn't need a NEAR wallet. Through NEAR Intents, USDC on Base, Ethereum, Solana or Tron lands in the hub as a funded contract in one step. That's a live quote from mainnet solvers: about sixty cents and thirty-five seconds. |
| 12 · **2:40–2:45** | SHARE CARD on Ledger Hand; holo tilt; then the X preview. | Every member has a card drawn from the live record, and a link that unfurls on X. |
| 13 · **2:46–3:00** | Back to the hero. Fade to repo URL + site URL. | Seals won in trials and burned on failure. Ranks the contract computes. Pay from escrow. Proofs verified on chain. Nothing here can be claimed, only earned. Guildhall, built on NEAR. |

The recorded voice-over (`video/voiceover.mp3`, ElevenLabs, voice "Daniel Calder") runs 3:00.4.
Times above are where each line starts and ends in that file, measured from the pauses;
the final "Guildhall. Built on NEAR." sits at 2:58–3:00. Lay each screen capture under its window.

## Production notes
- Voice-over: one continuous ElevenLabs take of this script, 3:00.4, with a beat between shots.
  Keep it as a single audio track; cut the video to the times above.
- Screen capture: the live site at night theme; the terminal in a dark window at 18 pt.
- Music: none, or something quiet and medieval under the hero and the close only.
- Export 1080p, upload unlisted on YouTube, put the link in the submission form.
