//! Live alignment against api.typesafe.ai: reads candidate pairs as
//! `a<TAB>b` lines on stdin, prints one verdict per line.
//!
//!   TYPESAFE_API_KEY=... cargo run --features jev --example jev_align < pairs.tsv

fn main() {
    let mut client = lemmalog::jev::JevClient::new(
        &std::env::var("JEV_MODEL").unwrap_or_else(|_| "jev-latest".into()),
    );
    let domain = "software infrastructure in Spanish (deployments, workers, \
                  databases, payroll, incidents)";
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input).expect("stdin");

    // A TAB means the caller already chose the pairs. No TAB anywhere means
    // it is a plain name list, so gate it first: all-pairs over a real
    // vocabulary is millions of calls, one per pair.
    let lines: Vec<&str> = input.lines().filter(|l| !l.trim().is_empty()).collect();
    let pairs: Vec<(String, String)> = if lines.iter().any(|l| l.contains('\t')) {
        lines
            .iter()
            .filter_map(|l| l.split_once('\t'))
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    } else {
        let names: Vec<String> = lines.iter().map(|l| l.trim().to_string()).collect();
        let min = std::env::var("JEV_MIN_JACCARD")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.5);
        let p = lemmalog::canonical::jev_align::candidate_pairs(&names, min);
        eprintln!("gate: {} names -> {} pairs (min_jaccard={min})", names.len(), p.len());
        p
    };

    let (mut merge, mut curator, mut leave) = (0, 0, 0);
    for (a, b) in &pairs {
        let (a, b) = (a.as_str(), b.as_str());
        match lemmalog::canonical::jev_align::align_pair(&mut client, domain, a, b) {
            Ok(v) => {
                use lemmalog::canonical::jev_align::Route::*;
                match v.route {
                    Merge => merge += 1,
                    Curator => curator += 1,
                    Leave => leave += 1,
                }
                println!(
                    "{:.2}\t{:.2}\t{:.2}\t{:?}\t{}\t{}",
                    v.score, v.confidence, v.noise_only, v.route, a, b
                );
            }
            Err(e) => eprintln!("FAIL\t{a}\t{b}\t{e}"),
        }
    }
    eprintln!(
        "calls={} failures={} merge={merge} curator={curator} leave={leave}",
        client.calls, client.failures
    );
}
