// Copyright 2016 Mozilla Foundation
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![allow(unused_imports, dead_code, unused_variables)]

use crate::compiler::args::*;
use crate::compiler::c::{CCompilerImpl, CCompilerKind, Language, ParsedArguments};
use crate::compiler::gcc::ArgData::*;
use crate::compiler::{gcc, write_temp_file, Cacheable, CompileCommand, CompilerArguments};
use crate::dist;
use crate::mock_command::{CommandCreator, CommandCreatorSync, RunCommand};
use crate::util::{run_input_output, OsStrExt};
use futures::future::{self, Future};
use futures_cpupool::CpuPool;
use log::Level::Trace;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;

use crate::errors::*;

/// A unit struct on which to implement `CCompilerImpl`.
#[derive(Clone, Debug)]
pub struct NVCC;

impl CCompilerImpl for NVCC {
    fn kind(&self) -> CCompilerKind {
        CCompilerKind::NVCC
    }
    fn parse_arguments(
        &self,
        arguments: &[OsString],
        cwd: &Path,
    ) -> CompilerArguments<ParsedArguments> {
        gcc::parse_arguments(arguments, cwd, (&gcc::ARGS[..], &ARGS[..]))
    }

    fn preprocess<T>(
        &self,
        creator: &T,
        executable: &Path,
        parsed_args: &ParsedArguments,
        cwd: &Path,
        env_vars: &[(OsString, OsString)],
        may_dist: bool,
        rewrite_includes_only: bool,
    ) -> SFuture<process::Output>
    where
        T: CommandCreatorSync,
    {
        trace!("preprocess");
        let mut cmd = creator.clone().new_command_sync(executable);

        //NVCC only supports `-E` when it comes after preprocessor
        //and common flags
        cmd.args(&parsed_args.preprocessor_args);
        cmd.args(&parsed_args.common_args);

        //We need to add "-rdc=true" if we are compiling with `-dc`
        //So that the preprocessor has the correct implicit defines
        if parsed_args.compilation_flag == "-dc" {
            cmd.arg("-rdc=true");
        }

        let language = match parsed_args.language {
            Language::C => "c",
            Language::Cxx => "c++",
            Language::ObjectiveC => "objective-c",
            Language::ObjectiveCxx => "objective-c++",
            Language::Cuda => "cu",
        };

        cmd.arg("-x").arg(language)
           .arg(&parsed_args.input)
           .arg("-E")
           .env_clear()
           .envs(env_vars.iter().map(|&(ref k, ref v)| (k, v)))
           .current_dir(cwd);
        if log_enabled!(Trace) {
            trace!("preprocess: {:?}", cmd);
        }
        run_input_output(cmd, None)
    }

    fn generate_compile_commands(
        &self,
        path_transformer: &mut dist::PathTransformer,
        executable: &Path,
        parsed_args: &ParsedArguments,
        cwd: &Path,
        env_vars: &[(OsString, OsString)],
        rewrite_includes_only: bool,
    ) -> Result<(CompileCommand, Option<dist::CompileCommand>, Cacheable)> {
        gcc::generate_compile_commands(
            path_transformer,
            executable,
            parsed_args,
            cwd,
            env_vars,
            self.kind(),
            rewrite_includes_only,
        )
    }
}

counted_array!(pub static ARGS: [ArgInfo<gcc::ArgData>; _] = [
    // take_arg!("--compiler-bindir", PathBuf, Separated, ExtraHashFile),
    // take_arg!("-ccbin", PathBuf, Separated, ExtraHashFile),

    take_arg!("--archive-options options", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("--compiler-options", OsString, CanBeSeparated('='), PreprocessorArgument),
    take_arg!("--generate-code", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("--gpu-architecture", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("--gpu-code", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("--linker-options", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("--maxrregcount", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("--nvlink-options", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("--ptxas-options", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("--relocatable-device-code", OsString, CanBeSeparated('='), PreprocessorArgument),

    take_arg!("-Xarchive", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("-Xcompiler", OsString, CanBeSeparated('='), PreprocessorArgument),
    take_arg!("-Xlinker", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("-Xnvlink", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("-Xptxas", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("-arch", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("-code", OsString, CanBeSeparated('='), PassThrough),
    flag!("-dc", DoCompilation),
    take_arg!("-gencode", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("-maxrregcount", OsString, CanBeSeparated('='), PassThrough),
    take_arg!("-rdc", OsString, CanBeSeparated('='), PreprocessorArgument),
    take_arg!("-x", OsString, CanBeSeparated('='), Language),

]);

#[cfg(test)]
mod test {
    use super::*;
    use crate::compiler::gcc;
    use crate::compiler::*;
    use crate::mock_command::*;
    use crate::test::utils::*;
    use futures::Future;
    use futures_cpupool::CpuPool;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn parse_arguments_(arguments: Vec<String>) -> CompilerArguments<ParsedArguments> {
        let arguments = arguments.iter().map(OsString::from).collect::<Vec<_>>();
        NVCC.parse_arguments(&arguments, ".".as_ref())
    }

    macro_rules! parses {
        ( $( $s:expr ),* ) => {
            match parse_arguments_(vec![ $( $s.to_string(), )* ]) {
                CompilerArguments::Ok(a) => a,
                o => panic!("Got unexpected parse result: {:?}", o),
            }
        }
    }

    #[test]
    fn test_parse_arguments_simple_c() {
        let a = parses!("-c", "foo.c", "-o", "foo.o");
        assert_eq!(Some("foo.c"), a.input.to_str());
        assert_eq!(Language::C, a.language);
        assert_map_contains!(a.outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, a.outputs.len());
        assert!(a.preprocessor_args.is_empty());
        assert!(a.common_args.is_empty());
    }

    #[test]
    fn test_parse_arguments_simple_cu() {
        let a = parses!("-c", "foo.cu", "-o", "foo.o");
        assert_eq!(Some("foo.cu"), a.input.to_str());
        assert_eq!(Language::Cuda, a.language);
        assert_map_contains!(a.outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, a.outputs.len());
        assert!(a.preprocessor_args.is_empty());
        assert!(a.common_args.is_empty());
    }

    #[test]
    fn test_parse_arguments_simple_c_as_cu() {
        let a = parses!("-x","cu", "-c", "foo.c", "-o", "foo.o");
        assert_eq!(Some("foo.c"), a.input.to_str());
        assert_eq!(Language::Cuda, a.language);
        assert_map_contains!(a.outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, a.outputs.len());
        assert!(a.preprocessor_args.is_empty());
        assert!(a.common_args.is_empty());
    }

    #[test]
    fn test_parse_arguments_dc_compile_flag() {
        let a = parses!("-x","cu", "-dc", "foo.c", "-o", "foo.o");
        assert_eq!(Some("foo.c"), a.input.to_str());
        assert_eq!(Language::Cuda, a.language);
        assert_eq!(Some("-dc"), a.compilation_flag.to_str());
        assert_map_contains!(a.outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, a.outputs.len());
        assert!(a.preprocessor_args.is_empty());
        assert!(a.common_args.is_empty());
    }

    #[test]
    fn test_parse_md_mt_flags_cu() {
        let a = parses!("-x", "cu", "-c", "foo.c", "-fabc",
                           "-MD", "-MT", "foo.o", "-MF", "foo.o.d", "-o", "foo.o");
        assert_eq!(Some("foo.c"), a.input.to_str());
        assert_eq!(Language::Cuda, a.language);
        assert_eq!(Some("-c"), a.compilation_flag.to_str());
        assert_map_contains!(a.outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, a.outputs.len());
        assert_eq!(
            ovec!["-MD", "-MF", "foo.o.d", "-MT", "foo.o"],
            a.preprocessor_args
        );
        assert_eq!(ovec!["-fabc"], a.common_args);
    }

    #[test]
    fn test_parse_generate_code_flags() {
        let a = parses!("-x","cu", "--generate-code=arch=compute_61,code=sm_61",
                        "-c", "foo.c", "-o", "foo.o");
        assert_eq!(Some("foo.c"), a.input.to_str());
        assert_eq!(Language::Cuda, a.language);
        assert_map_contains!(a.outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, a.outputs.len());
        assert!(a.preprocessor_args.is_empty());
        assert_eq!(
            ovec!["--generate-code", "arch=compute_61,code=sm_61"],
            a.common_args
        );
    }

    #[test]
    fn test_parse_pass_to_host_flags() {
        let a = parses!("-x=cu",
                        "--generate-code=arch=compute_60,code=[sm_60,sm_61]",
                        "-Xnvlink=--suppress-stack-size-warning",
                        "-Xcompiler", "-fPIC,-fno-common",
                        "-Xcompiler=-fvisibility=hidden",
                        "-Xcompiler=-Wall,-Wno-unknown-pragmas,-Wno-unused-local-typedefs",
                        "-Xcudafe", "--display_error_number",
                        "-c", "foo.c", "-o", "foo.o");
        assert_eq!(Some("foo.c"), a.input.to_str());
        assert_eq!(Language::Cuda, a.language);
        assert_map_contains!(a.outputs, ("obj", PathBuf::from("foo.o")));
        //TODO: fix assert_map_contains to assert no extra keys!
        assert_eq!(1, a.outputs.len());
        assert_eq!(
            ovec!["-Xcompiler", "-fPIC,-fno-common", "-Xcompiler", "-fvisibility=hidden",
                  "-Xcompiler", "-Wall,-Wno-unknown-pragmas,-Wno-unused-local-typedefs"],
            a.preprocessor_args
        );
        assert_eq!(
            ovec!["--generate-code", "arch=compute_60,code=[sm_60,sm_61]",
                  "-Xnvlink", "--suppress-stack-size-warning", "-Xcudafe",
                  "--display_error_number"],
            a.common_args
        );
    }

}
