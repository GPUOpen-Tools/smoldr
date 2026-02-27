#!/usr/bin/env python3
# Copyright (c) Advanced Micro Devices, Inc., or its affiliates.
#
# SPDX-License-Identifier: MIT OR Apache-2.0

import argparse
import os
import pathlib
import subprocess
import sys

def get_tests(args):
    result = []
    for path in args.tests:
        if not os.path.exists(path):
            raise ValueError(f"Path {path} does not exist!")
        if os.path.isdir(path):
            result += pathlib.Path(path).rglob("*.sm")
        else:
            result.append(path)
    print(f"Found {len(result)} tests")
    return result

def run(args):
    """Run tests and return the number of failures"""
    failed = []
    print(args.smoldr, args.arg)
    tests = get_tests(args)

    if args.quiet:
        output = subprocess.DEVNULL
    else:
        # Pass through
        output = None

    for path in tests:
        print(path)
        try:
            complete = subprocess.run([args.smoldr, *args.arg, path], timeout=args.timeout, stdout=output)

            if complete.returncode != 0:
                print("FAIL")
                failed.append(path)
            else:
                print("PASS")
        except subprocess.TimeoutExpired:
            print("FAIL; timeout")
            failed.append(path)

    if len(failed) > 0:
        print(f"There were {len(failed)} failures:")
        for f in failed:
            print("    " + str(f))

    return len(failed)

def get_args():
    parser = argparse.ArgumentParser(description="Run smoldr testing")
    parser.add_argument('smoldr', type=pathlib.Path, help="The smoldr binary to run")
    parser.add_argument('tests', nargs="+", default=[], help="Can be directories or test file names. For directories, all .sm files in the directory are used as test.")
    parser.add_argument('--quiet', '-q', action='store_true')
    parser.add_argument('-t', '--timeout', default="40", help="Change the timeout to the given value", type=int);
    parser.add_argument('-a', '--arg', nargs='*', default=[], help="Pass an argument to smoldr")

    return parser.parse_args()

def main():
    args = get_args()
    fails = run(args)
    if fails > 0:
        sys.exit(1)

if __name__ == '__main__':
    main()
