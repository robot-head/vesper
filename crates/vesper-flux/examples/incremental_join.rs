use vesper_flux::{CircuitBuilder, Frontier, Limits, TickInput, ZSet};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let l = Limits::default();
    let mut b = CircuitBuilder::new(l)?;
    let a = b.input(1)?;
    let c = b.input(1)?;
    let root = b.equijoin(a, c, 0, 0)?;
    let circuit = b.finish(root)?;
    let mut executor = circuit.clone().executor();
    let mut input = vec![ZSet::empty(1, l)?; 2];
    let mut output = ZSet::empty(2, l)?;
    let mut frontier = Frontier::new(0, 2)?;
    let mut ticket = frontier.acquire(0)?;
    for (tick, (wa, wb)) in [(2, 3), (-1, -2), (-1, -1)].into_iter().enumerate() {
        let deltas = vec![
            ZSet::consolidate(1, &[(vec![7], wa)], l)?,
            ZSet::consolidate(1, &[(vec![7], wb)], l)?,
        ];
        for i in 0..2 {
            input[i] = input[i].plus(&deltas[i], l)?;
        }
        let delta = executor.tick(&TickInput { deltas })?;
        output = output.plus(&delta, l)?;
        assert_eq!(output, circuit.batch(&input)?);
        ticket = frontier
            .advance(ticket, (tick + 1) as i64)
            .map_err(|e| e.error)?;
        println!(
            "tick {}: delta {:?}, integrated {:?}, frontier {}",
            tick + 1,
            delta,
            output,
            frontier.watermark()
        );
    }
    frontier.retire(ticket).map_err(|e| e.error)?;
    assert!(output.is_empty());
    assert_eq!(frontier.live_holds(), 0);
    Ok(())
}
