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


def validate_compiler_support(identity, support):
    """Accept any stable or nightly compiler at or above the compiler adapter's floor.

    The driver is built for, and bound to, the exact compiler selected here, so a newer stable release or
    nightly works as well as the repository pin. Release archives separately require the pin.
    """
    if (_release_key(identity['release']) < _release_key(support['minimum_release'])
            or _date_key(identity['commit-date']) < _date_key(support['minimum_commit_date'])):
        raise ValueError(
            'compiler is below the compiler adapter minimum: '
            f'expected rustc {support["minimum_release"]} or newer '
            f'from {support["minimum_commit_date"]} or later; '
            f'found rustc {identity["release"]} ({identity["commit-date"]})'
        )
