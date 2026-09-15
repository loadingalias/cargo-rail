"""Validate the compiler selected to build an authenticated fact driver."""

from datetime import date
import re


_RELEASE = re.compile(
    r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
    r'(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?\Z'
)


def _release_key(value):
    match = _RELEASE.fullmatch(value)
    if match is None:
        raise ValueError(f'invalid rustc release: {value}')
    prerelease = []
    for identifier in (match.group(4) or '').split('.'):
        if not identifier:
            continue
        if identifier.isdigit():
            if len(identifier) > 1 and identifier.startswith('0'):
                raise ValueError(f'invalid rustc release: {value}')
            prerelease.append((0, int(identifier)))
        else:
            prerelease.append((1, identifier))
    return (*(int(part) for part in match.groups()[:3]), match.group(4) is None, tuple(prerelease))


def _date_key(value):
    try:
        parsed = date.fromisoformat(value)
    except ValueError as error:
        raise ValueError(f'invalid rustc commit date: {value}') from error
    if parsed.isoformat() != value:
        raise ValueError(f'invalid rustc commit date: {value}')
    return parsed


def validate_compiler_support(identity, support, target, native_release):
    selected_release = _release_key(identity['release'])
    minimum_release = _release_key(support['minimum_release'])
    maximum_release = _release_key(support['maximum_release'])
    native_release_key = _release_key(native_release)
    selected_date = _date_key(identity['commit-date'])
    minimum_date = _date_key(support['minimum_commit_date'])
    maximum_date = _date_key(support['maximum_commit_date'])
    if minimum_release > maximum_release or minimum_date > maximum_date:
        raise ValueError('fact-driver source support interval is invalid')
    if not minimum_release <= native_release_key <= maximum_release:
        raise ValueError('native release compiler is outside the fact-driver source support interval')
    if target == identity['host']:
        if selected_release != native_release_key or selected_date != maximum_date:
            raise ValueError(
                'native release compiler must match rust-toolchain.toml and the support maximum date: '
                f'expected rustc {native_release} ({support["maximum_commit_date"]}); '
                f'found rustc {identity["release"]} ({identity["commit-date"]})'
            )
    elif not (minimum_release <= selected_release <= maximum_release
              and minimum_date <= selected_date <= maximum_date):
        raise ValueError(
            'cross compiler is outside the fact-driver source support interval: '
            f'expected rustc {support["minimum_release"]} through {support["maximum_release"]} '
            f'from {support["minimum_commit_date"]} through {support["maximum_commit_date"]}; '
            f'found rustc {identity["release"]} ({identity["commit-date"]})'
        )
