//! What a session used: tokens, what they'd cost at API prices whatever plan paid for them, and
//! the skills and MCP servers it called. Prices come from the list Pi keeps in
//! `models-store.json`. Parsers only note each request, because long-context rates depend on
//! that one request's size, so a total can't be priced.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::data::{Cursor, Roots, Session, Source, is_compressed, read_records};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Tokens {
    pub fn total(self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// Dollars.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Costs {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Costs {
    pub fn total(self) -> f64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

/// One call to a model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    pub model: String,
    pub tokens: Tokens,
    /// How much of `tokens.cache_write` went to Anthropic's one-hour cache.
    pub cache_write_1h: u64,
    pub fast: bool,
    /// What the tool itself recorded, at the prices of the day. Only Pi does.
    pub stored: Option<Costs>,
}

/// Dollars per million tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Rates {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

impl Rates {
    fn parse(cost: &Value) -> Rates {
        let rate = |key: &str| cost[key].as_f64().unwrap_or(0.0);
        Rates { input: rate("input"), output: rate("output"), cache_read: rate("cacheRead"), cache_write: rate("cacheWrite") }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Price {
    base: Rates,
    /// Rates for a request whose whole prompt is over a number of tokens.
    tiers: Vec<(u64, Rates)>,
}

impl Price {
    fn parse(cost: &Value) -> Price {
        let tiers = cost["tiers"].as_array().map(Vec::as_slice).unwrap_or_default();
        Price { base: Rates::parse(cost), tiers: tiers.iter().filter_map(|t| Some((t["inputTokensAbove"].as_u64()?, Rates::parse(t)))).collect() }
    }

    fn cost(&self, request: &Request) -> Costs {
        let t = request.tokens;
        let prompt = t.input + t.cache_read + t.cache_write;
        let rates = self.tiers.iter().filter(|(above, _)| prompt > *above).max_by_key(|(above, _)| *above).map_or(self.base, |(_, r)| *r);
        // Fast mode is twice the price, and the cache's multipliers apply on top of it.
        let scale = if request.fast { 2.0 } else { 1.0 };
        let dollars = |tokens: u64, rate: f64| tokens as f64 * rate * scale / 1e6;
        let long = request.cache_write_1h.min(t.cache_write);
        Costs {
            input: dollars(t.input, rates.input),
            output: dollars(t.output, rates.output),
            cache_read: dollars(t.cache_read, rates.cache_read),
            // A one-hour write is twice the base input price, a rate the list leaves out.
            cache_write: dollars(t.cache_write - long, rates.cache_write) + dollars(long, rates.input * 2.0),
        }
    }
}

/// Each provider's models. A model listed twice under a provider keeps its first entry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prices(HashMap<String, HashMap<String, Price>>);

impl Prices {
    /// The list beside Pi's sessions, or none where Pi isn't installed.
    pub fn load(roots: &Roots) -> Prices {
        let path = roots.get(Source::Pi).parent().map(|agent| agent.join("models-store.json")).unwrap_or_default();
        std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).map_or_else(Prices::default, |list| Prices::parse(&list))
    }

    fn parse(list: &Value) -> Prices {
        let mut prices: HashMap<String, HashMap<String, Price>> = HashMap::new();
        for (provider, entry) in list.as_object().into_iter().flatten() {
            let models = prices.entry(provider.clone()).or_default();
            for model in entry["models"].as_array().into_iter().flatten() {
                if let Some(id) = model["id"].as_str() {
                    models.entry(id.to_owned()).or_insert_with(|| Price::parse(&model["cost"]));
                }
            }
        }
        Prices(prices)
    }

    fn find(&self, source: Source, model: &str) -> Option<&Price> {
        source.agent().providers().iter().find_map(|provider| self.0.get(*provider)?.get(model))
    }
}

/// What one transcript file used, as far as it's been read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    requests: Vec<Request>,
    skills: BTreeMap<String, usize>,
    mcp: BTreeMap<String, BTreeMap<String, usize>>,
    /// Requests and calls already counted: Claude writes a reply's usage on each of its blocks.
    seen: HashSet<String>,
    /// The model a Codex file is on, which its usage records don't name.
    pub(crate) model: String,
    /// Codex's running total, which it repeats when nothing's been used.
    pub(crate) running: u64,
    /// Whether a Codex file has per-request records, which replace its older running totals.
    pub(crate) recorded: bool,
}

impl Usage {
    /// Adds a request. One with an `id` is counted once.
    pub fn request(&mut self, id: &str, request: Request) {
        if id.is_empty() || self.seen.insert(id.to_owned()) {
            self.requests.push(request);
        }
    }

    pub fn skill(&mut self, name: &str) {
        *self.skills.entry(name.to_owned()).or_default() += 1;
    }

    /// Adds a call to an MCP server's tool. One with an `id` is counted once.
    pub fn mcp(&mut self, id: &str, server: &str, tool: &str) {
        if id.is_empty() || self.seen.insert(id.to_owned()) {
            *self.mcp.entry(server.to_owned()).or_default().entry(tool.to_owned()).or_default() += 1;
        }
    }

    /// Takes in a tool called `mcp__<server>__<tool>`.
    pub fn mcp_tool(&mut self, id: &str, name: &str) {
        if let Some((server, tool)) = name.strip_prefix("mcp__").and_then(|n| n.split_once("__")) {
            self.mcp(id, server, tool);
        }
    }

    /// Takes in a call's input, counting each skill whose `SKILL.md` it names. It's how Codex
    /// and Pi use a skill, with no call of its own to go by.
    pub fn skills_read(&mut self, input: &str) {
        let mut names: Vec<&str> = input.match_indices("/SKILL.md").filter_map(|(at, _)| skill_name(&input[..at])).collect();
        names.sort_unstable();
        names.dedup();
        for name in names {
            self.skill(name);
        }
    }

    /// Names the requests made before a file said which model it was on.
    pub(crate) fn name_model(&mut self, model: &str) {
        if self.model.is_empty() {
            self.requests.iter_mut().filter(|r| r.model.is_empty()).for_each(|r| r.model = model.to_owned());
        }
        self.model = model.to_owned();
    }
}

/// The skill whose folder ends this text: an absolute path through a `skills` folder. Commands
/// also mention `SKILL.md` in examples and fixtures, which relative paths and URLs mostly are.
fn skill_name(text: &str) -> Option<&str> {
    let word = text.rsplit(|c: char| c.is_whitespace() || "\"'`()=,[]<>“”‘’".contains(c)).next()?;
    let (folders, name) = word.strip_prefix('~').unwrap_or(word).strip_prefix('/')?.rsplit_once('/')?;
    (!name.is_empty() && folders.split('/').any(|folder| folder == "skills")).then_some(name)
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Agent {
    read: Cursor,
    usage: Usage,
}

/// A session's subagents, whose transcripts are files of their own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Agents {
    files: BTreeMap<PathBuf, Agent>,
    /// Codex files already found not to be this session's.
    others: HashSet<PathBuf>,
}

impl Agents {
    /// These agents with what's been added to their files, or the same ones if nothing has.
    pub fn advanced(agents: &Arc<Agents>, session: &Session) -> Arc<Agents> {
        let Some((found, others)) = session.source.agent().subagent_files(session, &agents.others) else { return agents.clone() };
        // Codex compresses a quiet file and removes the original, so both can exist for a moment.
        // The original stands for its compressed copy, whether it's been read or is about to be.
        let original = |path: &PathBuf| is_compressed(path).then(|| path.with_extension(""));
        let known = |path: &PathBuf| agents.files.contains_key(path) || original(path).is_some_and(|plain| agents.files.contains_key(&plain) || found.contains(&plain));
        let found: Vec<PathBuf> = found.iter().filter(|path| !known(path)).cloned().collect();
        let grown = |(path, agent): (&PathBuf, &Agent)| has_more(path, agent.read);
        if found.is_empty() && others.is_empty() && !agents.files.iter().any(grown) {
            return agents.clone();
        }
        let mut next = (**agents).clone();
        next.others.extend(others);
        next.files.extend(found.into_iter().map(|path| (path, Agent::default())));
        for (path, agent) in &mut next.files {
            if !has_more(path, agent.read) {
                continue;
            }
            let usage = &mut agent.usage;
            let add = |_: usize, record: Value| session.source.agent().add_usage(usage, &record);
            if let Ok(read) = read_records(path, agent.read, |line| session.source.agent().wanted(line), add) {
                agent.read = read;
            }
        }
        Arc::new(next)
    }
}

/// Whether a file holds more than has been read. A compressed one doesn't grow.
fn has_more(path: &Path, read: Cursor) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.len() != read.offset && !(is_compressed(path) && read.offset > 0))
}

/// Tokens and what they cost.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spend {
    pub tokens: Tokens,
    pub cost: Costs,
    /// How many of `tokens` are on a model with no price.
    pub unpriced: u64,
}

impl Spend {
    fn add(&mut self, request: &Request, cost: Option<Costs>) {
        let t = request.tokens;
        self.tokens.input += t.input;
        self.tokens.output += t.output;
        self.tokens.cache_read += t.cache_read;
        self.tokens.cache_write += t.cache_write;
        let Some(cost) = cost else {
            self.unpriced += t.total();
            return;
        };
        self.cost.input += cost.input;
        self.cost.output += cost.output;
        self.cost.cache_read += cost.cache_read;
        self.cost.cache_write += cost.cache_write;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    pub name: String,
    pub calls: usize,
    pub tools: Vec<(String, usize)>,
}

/// A session and its subagents, for showing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub total: Spend,
    /// The share of `total` that was subagents'.
    pub agents: Spend,
    pub models: Vec<(String, Spend)>,
    pub skills: Vec<(String, usize)>,
    pub mcp: Vec<Server>,
}

impl Report {
    pub fn new(source: Source, usage: &Usage, agents: &Agents, prices: &Prices) -> Report {
        let mut report = Report::default();
        let mut models: BTreeMap<&str, Spend> = BTreeMap::new();
        let mut skills: BTreeMap<&str, usize> = BTreeMap::new();
        let mut mcp: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
        let files = agents.files.values().map(|a| (&a.usage, true));
        for (usage, agent) in std::iter::once((usage, false)).chain(files) {
            for request in &usage.requests {
                let cost = request.stored.or_else(|| Some(prices.find(source, &request.model)?.cost(request)));
                report.total.add(request, cost);
                if agent {
                    report.agents.add(request, cost);
                }
                models.entry(&request.model).or_default().add(request, cost);
            }
            for (name, calls) in &usage.skills {
                *skills.entry(name).or_default() += calls;
            }
            for (server, tools) in &usage.mcp {
                let counts = mcp.entry(server).or_default();
                for (tool, calls) in tools {
                    *counts.entry(tool).or_default() += calls;
                }
            }
        }
        let name = |model: &str| if model.is_empty() { "Unknown".to_owned() } else { model.to_owned() };
        report.models = models.into_iter().map(|(model, spend)| (name(model), spend)).collect();
        report.models.sort_by(|a, b| b.1.cost.total().total_cmp(&a.1.cost.total()).then_with(|| b.1.tokens.total().cmp(&a.1.tokens.total())));
        report.skills = most_first(skills);
        report.mcp = mcp
            .into_iter()
            .map(|(name, tools)| {
                let tools = most_first(tools);
                Server { name: name.to_owned(), calls: tools.iter().map(|(_, calls)| calls).sum(), tools }
            })
            .collect();
        report.mcp.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.name.cmp(&b.name)));
        report
    }
}

fn most_first(counts: BTreeMap<&str, usize>) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = counts.into_iter().map(|(name, calls)| (name.to_owned(), calls)).collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// The last component of a skill's folder, from Claude's `Base directory for this skill: <path>`.
pub fn folder_name(path: &str) -> Option<&str> {
    Path::new(path.trim()).file_name()?.to_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prices() -> Prices {
        Prices::parse(&json!({
            "anthropic": {"models": [
                {"id": "opus", "cost": {"input": 4, "output": 20, "cacheRead": 0.2, "cacheWrite": 5}},
                {"id": "haiku", "cost": {"input": 0.1, "output": 0.5, "cacheRead": 0.01, "cacheWrite": 0.125,
                    "tiers": [{"inputTokensAbove": 100_000, "input": 0.5, "output": 2.5, "cacheRead": 0.05, "cacheWrite": 0.625}]}},
            ]},
            "openai": {"models": [
                {"id": "gpt", "cost": {"input": 9, "output": 9, "cacheRead": 9, "cacheWrite": 9}},
                {"id": "only-api", "cost": {"input": 1, "output": 1, "cacheRead": 1, "cacheWrite": 1}},
            ]},
            "openai-codex": {"models": [
                {"id": "gpt", "cost": {"input": 2, "output": 10, "cacheRead": 0.1, "cacheWrite": 2.5}},
                {"id": "gpt", "cost": {"input": 2, "output": 0, "cacheRead": 0, "cacheWrite": 0}},
            ]},
        }))
    }

    fn request(model: &str, input: u64, output: u64, cache_read: u64, cache_write: u64) -> Request {
        Request { model: model.to_owned(), tokens: Tokens { input, output, cache_read, cache_write }, ..Request::default() }
    }

    fn cost(source: Source, r: &Request) -> f64 {
        prices().find(source, &r.model).unwrap().cost(r).total()
    }

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "{a} != {b}");
    }

    #[test]
    fn each_kind_of_token_has_its_own_rate() {
        let costs = prices().find(Source::Claude, "opus").unwrap().cost(&request("opus", 1_000_000, 1_000_000, 1_000_000, 1_000_000));
        assert_eq!(costs, Costs { input: 4.0, output: 20.0, cache_read: 0.2, cache_write: 5.0 });
    }

    #[test]
    fn a_one_hour_cache_write_is_twice_the_base_input_rate() {
        let mut r = request("opus", 0, 0, 0, 1_000_000);
        r.cache_write_1h = 400_000;
        close(cost(Source::Claude, &r), 0.6 * 5.0 + 0.4 * 8.0);
    }

    #[test]
    fn fast_mode_doubles_every_rate() {
        let mut r = request("opus", 1_000_000, 1_000_000, 0, 0);
        r.fast = true;
        close(cost(Source::Claude, &r), 48.0);
    }

    #[test]
    fn long_context_rates_go_by_the_whole_prompt_of_that_one_request() {
        close(cost(Source::Claude, &request("haiku", 40_000, 0, 60_000, 0)), 0.004 + 0.0006);
        close(cost(Source::Claude, &request("haiku", 40_000, 0, 60_000, 1)), 0.02 + 0.003 + 0.000000625);
    }

    #[test]
    fn two_small_requests_arent_priced_as_one_big_one() {
        let mut usage = Usage::default();
        usage.request("", request("haiku", 80_000, 0, 0, 0));
        usage.request("", request("haiku", 80_000, 0, 0, 0));
        close(Report::new(Source::Claude, &usage, &Agents::default(), &prices()).total.cost.total(), 0.016);
    }

    #[test]
    fn codex_prefers_its_own_provider_and_a_models_first_entry() {
        close(cost(Source::Codex, &request("gpt", 0, 1_000_000, 0, 0)), 10.0);
        close(cost(Source::Codex, &request("only-api", 0, 1_000_000, 0, 0)), 1.0);
        assert!(prices().find(Source::Claude, "gpt").is_none());
    }

    #[test]
    fn a_stored_cost_is_kept_and_a_model_with_no_price_isnt_called_free() {
        let mut usage = Usage::default();
        let mut stored = request("gpt", 1_000_000, 0, 0, 0);
        stored.stored = Some(Costs { input: 7.0, ..Costs::default() });
        usage.request("", stored);
        usage.request("", request("mystery", 300, 200, 0, 0));
        let report = Report::new(Source::Pi, &usage, &Agents::default(), &prices());
        close(report.total.cost.total(), 7.0);
        assert_eq!(report.total.unpriced, 500);
        assert_eq!(report.total.tokens.total(), 1_000_500);
    }

    #[test]
    fn a_request_with_an_id_is_counted_once() {
        let mut usage = Usage::default();
        usage.request("m", request("opus", 1, 0, 0, 0));
        usage.request("m", request("opus", 1, 0, 0, 0));
        usage.request("", request("opus", 1, 0, 0, 0));
        usage.request("", request("opus", 1, 0, 0, 0));
        assert_eq!(usage.requests.len(), 3);
    }

    #[test]
    fn subagents_add_to_the_total_and_show_their_share() {
        let mut usage = Usage::default();
        usage.request("", request("opus", 1_000_000, 0, 0, 0));
        usage.skill("deep");
        usage.mcp("", "web", "open");
        let mut agent = Usage::default();
        agent.request("", request("haiku", 1_000_000, 0, 0, 0));
        agent.skill("deep");
        agent.mcp("", "web", "open");
        agent.mcp("", "web", "click");
        let agents = Agents { files: BTreeMap::from([(PathBuf::from("a"), Agent { read: Cursor::default(), usage: agent })]), others: HashSet::new() };
        let report = Report::new(Source::Claude, &usage, &agents, &prices());
        assert_eq!(report.total.tokens.input, 2_000_000);
        assert_eq!(report.agents.tokens.input, 1_000_000);
        close(report.agents.cost.total(), 0.5);
        assert_eq!(report.models.iter().map(|m| m.0.as_str()).collect::<Vec<_>>(), ["opus", "haiku"]);
        assert_eq!(report.skills, [("deep".to_owned(), 2)]);
        assert_eq!(report.mcp, [Server { name: "web".to_owned(), calls: 3, tools: vec![("open".to_owned(), 2), ("click".to_owned(), 1)] }]);
    }

    #[test]
    fn a_skill_is_known_by_the_folder_of_the_file_read() {
        let mut usage = Usage::default();
        usage.skills_read("sed -n 1,200p /Users/a/.agents/skills/gh-cli/SKILL.md && cat '/x/skills/rex/SKILL.md' ~/.agents/skills/gh-cli/SKILL.md /x/skills/.system/deep/SKILL.md");
        assert_eq!(usage.skills, BTreeMap::from([("deep".to_owned(), 1), ("gh-cli".to_owned(), 1), ("rex".to_owned(), 1)]));
    }

    #[test]
    fn a_skill_file_with_no_folder_names_no_skill() {
        let mut usage = Usage::default();
        usage.skills_read("cat SKILL.md; echo <name>/SKILL.md; cat ./SKILL.md; cat /repo/fixtures/alpha/SKILL.md");
        usage.skills_read("cat “docs/SKILL.md cat\u{3000}/SKILL.md \u{a0}x/SKILL.md");
        usage.skills_read("git clone https://example.com/skills/repo/SKILL.md; cat testdata/skills/one/SKILL.md; ssh://user:password@host/skills/x/SKILL.md");
        assert_eq!(usage.skills, BTreeMap::new());
    }

    #[test]
    fn requests_from_before_the_model_was_known_take_it() {
        let mut usage = Usage::default();
        usage.request("", request("", 1, 0, 0, 0));
        usage.name_model("gpt");
        usage.request("", request("gpt", 1, 0, 0, 0));
        usage.name_model("other");
        assert!(usage.requests.iter().all(|r| r.model == "gpt"));
    }
}
