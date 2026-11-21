"""Descriptive, matched-completed-turn comparison; no independent-sample inference."""
import argparse
import json
from statistics import median


def matching_inventory(before, after):
    def observed(rows):
        inventory = None
        for row in rows:
            current = {key: row['mock'][key] for key in
                       ('inventory_sha256', 'inventory_bytes', 'inventory_count')}
            if not any(current.values()) and inventory is None:
                continue  # A sampler can run before the first provider request.
            if (current['inventory_count'] != 36 or current['inventory_bytes'] <= 0
                    or not current['inventory_sha256']):
                raise ValueError('invalid or reset observed inventory')
            if inventory is not None and current != inventory:
                raise ValueError('effective MCP inventory drift within artifact')
            inventory = current
        if inventory is None:
            raise ValueError('no effective MCP inventory observed')
        return inventory
    a, b = observed(before), observed(after)
    if a != b:
        raise ValueError('effective provider MCP inventory differs across runs')
    return a


def slope(rows, x, y):
    xs, ys = [r[x] for r in rows], [r[y] for r in rows]
    mx, my = sum(xs) / len(xs), sum(ys) / len(ys)
    denominator = sum((v - mx) ** 2 for v in xs)
    if not denominator:
        raise ValueError('insufficient variation for slope')
    return sum((a - mx) * (b - my) for a, b in zip(xs, ys)) / denominator


def summarize(rows):
    result = {'samples': len(rows), 'elapsed_range_s': [rows[0]['elapsed_s'], rows[-1]['elapsed_s']]}
    for key in ('rss_kib', 'pss_kib'):
        result[key + '_median'] = median(r[key] for r in rows)
        result[key + '_per_turn'] = slope(rows, 'completed_turns', key)
        result[key + '_per_second'] = slope(rows, 'elapsed_s', key)
        result[key + '_first'] = rows[0][key]
        result[key + '_last'] = rows[-1][key]
    return result


def compare(before, after, warmup_before, warmup_after):
    a = [r for r in before if r['elapsed_s'] >= warmup_before]
    b = [r for r in after if r['elapsed_s'] >= warmup_after]
    if len(a) < 3 or len(b) < 3:
        raise ValueError('need at least three post-warmup samples in each run')
    low = max(a[0]['completed_turns'], b[0]['completed_turns'])
    high = min(a[-1]['completed_turns'], b[-1]['completed_turns'])
    a = [r for r in a if low <= r['completed_turns'] <= high]
    b = [r for r in b if low <= r['completed_turns'] <= high]
    if high <= low or len(a) < 3 or len(b) < 3:
        raise ValueError('insufficient overlapping completed-turn range')
    return {'completed_turn_range': [low, high], 'before': summarize(a), 'after': summarize(b),
            'interpretation': 'Descriptive single-run slopes over overlapping work after both warmups. '
            'Time-series samples are autocorrelated; no confidence intervals or p-values. '
            'No baseline growth means not reproduced, not proven fixed.'}


def load(path):
    samples, metadata, complete = [], None, False
    with open(path) as stream:
        for line in stream:
            row = json.loads(line)
            if row['kind'] == 'error':
                raise ValueError(f'{path}: failed run cannot be compared')
            if row['kind'] == 'metadata':
                if metadata is not None:
                    raise ValueError('multiple runs in one artifact')
                metadata = row
            elif row['kind'] == 'sample':
                samples.append(row)
            elif row['kind'] == 'complete':
                complete = True
    if metadata is None or not complete:
        raise ValueError(f'{path}: missing metadata or completion marker')
    return metadata, samples


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('before')
    parser.add_argument('after')
    args = parser.parse_args()
    ma, a = load(args.before)
    mb, b = load(args.after)
    if ma['workload'] != mb['workload']:
        raise ValueError('workload metadata differs')
    inventory = matching_inventory(a, b)
    for key in ('sample_seconds', 'turn_timeout', 'duration_seconds', 'warmup_seconds'):
        if ma['settings'][key] != mb['settings'][key]:
            raise ValueError(f'unmatched setting: {key}')
    result = compare(a, b, ma['settings']['warmup_seconds'], mb['settings']['warmup_seconds'])
    result['revisions'] = [ma['revision'], mb['revision']]
    result['binary_sha256'] = [ma['binary_sha256'], mb['binary_sha256']]
    result['inventory'] = inventory
    result['wall_time_post_warmup'] = {
        'before': summarize([r for r in a if r['elapsed_s'] >= ma['settings']['warmup_seconds']]),
        'after': summarize([r for r in b if r['elapsed_s'] >= mb['settings']['warmup_seconds']])}
    print(json.dumps(result, indent=2, sort_keys=True))
