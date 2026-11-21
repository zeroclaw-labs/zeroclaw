// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use std::env;
use std::io::{self, Read};
use xmpp_parsers::{
    caps::{compute_disco as compute_disco_caps, hash_caps, Caps},
    disco::DiscoInfoResult,
    ecaps2::{compute_disco as compute_disco_ecaps2, hash_ecaps2, ECaps2},
    hashes::Algo,
    minidom::Element,
    Error,
};

fn get_caps(disco: &DiscoInfoResult, node: String) -> Result<Caps, String> {
    let data = compute_disco_caps(&disco);
    let hash = hash_caps(&data, Algo::Sha_1)?;
    Ok(Caps::new(node, hash))
}

fn get_ecaps2(disco: &DiscoInfoResult) -> Result<ECaps2, Error> {
    let data = compute_disco_ecaps2(&disco)?;
    let hash_sha256 = hash_ecaps2(&data, Algo::Sha_256)?;
    let hash_sha3_256 = hash_ecaps2(&data, Algo::Sha3_256)?;
    let hash_blake2b_256 = hash_ecaps2(&data, Algo::Blake2b_256)?;
    Ok(ECaps2::new(vec![
        hash_sha256,
        hash_sha3_256,
        hash_blake2b_256,
    ]))
}

fn main() -> Result<(), Box<dyn core::error::Error>> {
    let args: Vec<_> = env::args().collect();
    if args.len() != 2 {
        println!("Usage: {} <node>", args[0]);
        std::process::exit(1);
    }
    let node = args[1].clone();

    eprintln!("Reading a disco#info payload from stdin...");

    // Read from stdin.
    let stdin = io::stdin();
    let mut data = String::new();
    let mut handle = stdin.lock();
    handle.read_to_string(&mut data)?;

    // Parse the payload into a DiscoInfoResult.
    let elem: Element = data.parse()?;
    let disco = DiscoInfoResult::try_from(elem)?;

    // Compute both kinds of caps.
    let caps = get_caps(&disco, node)?;
    let ecaps2 = get_ecaps2(&disco)?;

    // Print them.
    let caps_elem = Element::from(caps);
    let ecaps2_elem = Element::from(ecaps2);
    println!("{}", String::from(&caps_elem));
    println!("{}", String::from(&ecaps2_elem));

    Ok(())
}
