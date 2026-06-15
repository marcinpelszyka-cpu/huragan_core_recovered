#!/usr/bin/env node
/*
 * Execute a previously generated dust cleanup plan.
 *
 * Safe default: dry-run only. To actually sign/broadcast:
 *   node scripts/dust_cleaner_execute.js --execute --yes CLEAN_DUST
 *
 * The script never prints private keys or RPC URLs.
 */
const fs = require('fs');
const path = require('path');
process.env.NODE_PATH = ['/tmp/node_modules', process.env.NODE_PATH || ''].filter(Boolean).join(path.delimiter);
require('module').Module._initPaths();
const {
  Connection,
  Keypair,
  PublicKey,
  Transaction,
  sendAndConfirmTransaction,
} = require('@solana/web3.js');
const {
  createCloseAccountInstruction,
  TOKEN_PROGRAM_ID,
  TOKEN_2022_PROGRAM_ID,
} = require('@solana/spl-token');
const mod = require('bs58');
const bs58 = mod.default || mod;

const LAMPORTS = 1_000_000_000;
const DEFAULT_PLAN = 'datasets/dust_cleaner_plan.json';
const DEFAULT_WALLET_ENV = '/root/.huragan_wallets/huragan_new_wallet_20260604_003229.env';

function parseArgs(argv) {
  const args = {
    plan: DEFAULT_PLAN,
    walletEnv: DEFAULT_WALLET_ENV,
    projectEnv: '.env',
    execute: false,
    yes: '',
    batchSize: 8,
  };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--plan') args.plan = argv[++i];
    else if (a === '--wallet-env') args.walletEnv = argv[++i];
    else if (a === '--project-env') args.projectEnv = argv[++i];
    else if (a === '--execute') args.execute = true;
    else if (a === '--yes') args.yes = argv[++i] || '';
    else if (a === '--batch-size') args.batchSize = Number(argv[++i] || '8');
    else if (a === '--help' || a === '-h') usage(0);
    else throw new Error(`unknown arg: ${a}`);
  }
  return args;
}

function usage(code) {
  console.log(`Usage:
  node scripts/dust_cleaner_execute.js [--plan datasets/dust_cleaner_plan.json]
  node scripts/dust_cleaner_execute.js --execute --yes CLEAN_DUST

Options:
  --plan PATH          cleanup plan from dust_cleaner_plan.py
  --wallet-env PATH    wallet env containing SOLANA_PRIVATE_KEY_BASE58
  --project-env PATH   project .env with RPC_SEND_URL/HELIUS_RPC_URL
  --batch-size N       close instructions per tx (default 8)
  --execute            sign and broadcast
  --yes CLEAN_DUST     required exact confirmation for --execute
`);
  process.exit(code);
}

function parseEnv(file) {
  const out = {};
  if (!fs.existsSync(file)) return out;
  for (const raw of fs.readFileSync(file, 'utf8').split('\n')) {
    const s = raw.trim();
    if (!s || s.startsWith('#')) continue;
    const eq = s.indexOf('=');
    if (eq < 0) continue;
    out[s.slice(0, eq).trim()] = s.slice(eq + 1).trim().replace(/^["']|["']$/g, '');
  }
  return out;
}

function rpcUrl(projectEnv) {
  const env = parseEnv(projectEnv);
  for (const k of ['RPC_SEND_URL', 'HELIUS_RPC_URL', 'RPC_URL']) {
    if (env[k]) return env[k];
  }
  throw new Error('RPC URL missing in project env');
}

function loadKeypair(walletEnv) {
  const env = parseEnv(walletEnv);
  if (!env.SOLANA_PRIVATE_KEY_BASE58) throw new Error(`SOLANA_PRIVATE_KEY_BASE58 missing in ${walletEnv}`);
  return Keypair.fromSecretKey(bs58.decode(env.SOLANA_PRIVATE_KEY_BASE58));
}

function programId(id) {
  if (id === TOKEN_PROGRAM_ID.toBase58()) return TOKEN_PROGRAM_ID;
  if (id === TOKEN_2022_PROGRAM_ID.toBase58()) return TOKEN_2022_PROGRAM_ID;
  throw new Error(`unsupported token program in plan: ${id}`);
}

function short(s) { return `${s.slice(0, 6)}...${s.slice(-4)}`; }

async function main() {
  const args = parseArgs(process.argv);
  const plan = JSON.parse(fs.readFileSync(args.plan, 'utf8'));
  const actions = (plan.actions || []).filter(a =>
    a.kind === 'unwrap_wsol_close_account' || a.kind === 'close_empty_token_account'
  );
  const recover = actions.reduce((n, a) => n + Number(a.recover_lamports || 0), 0);
  console.log('DUST CLEANER EXECUTOR');
  console.log(`owner=${plan.owner_short || short(plan.owner || '')}`);
  console.log(`actions=${actions.length}`);
  console.log(`recoverable_SOL≈${(recover / LAMPORTS).toFixed(9)}`);

  if (!args.execute) {
    console.log('mode=DRY_RUN');
    console.log('No transaction signed. Use --execute --yes CLEAN_DUST after operator approval.');
    return;
  }
  if (args.yes !== 'CLEAN_DUST') throw new Error('refusing execute: pass --yes CLEAN_DUST exactly');
  if (!actions.length) {
    console.log('nothing_to_close');
    return;
  }

  const kp = loadKeypair(args.walletEnv);
  if (plan.owner && kp.publicKey.toBase58() !== plan.owner) {
    throw new Error(`wallet mismatch: key=${short(kp.publicKey.toBase58())} plan=${short(plan.owner)}`);
  }
  const conn = new Connection(rpcUrl(args.projectEnv), 'confirmed');
  const sigs = [];
  for (let i = 0; i < actions.length; i += args.batchSize) {
    const batch = actions.slice(i, i + args.batchSize);
    const tx = new Transaction();
    for (const a of batch) {
      tx.add(createCloseAccountInstruction(
        new PublicKey(a.token_account),
        kp.publicKey,
        kp.publicKey,
        [],
        programId(a.program_id),
      ));
    }
    const sig = await sendAndConfirmTransaction(conn, tx, [kp], { commitment: 'confirmed' });
    sigs.push(sig);
    console.log(`sent_batch=${Math.floor(i / args.batchSize) + 1} actions=${batch.length} sig=${sig}`);
  }
  const bal = await conn.getBalance(kp.publicKey, 'confirmed');
  console.log(`done signatures=${sigs.length} final_main_SOL=${(bal / LAMPORTS).toFixed(9)}`);
}

main().catch((err) => {
  console.error(`ERROR: ${err.message}`);
  process.exit(1);
});
