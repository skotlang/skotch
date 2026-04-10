; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [6 x i8] c"world\00", align 1
@.str.1 = private unnamed_addr constant [8 x i8] c"Hello, \00", align 1
@.str.2 = private unnamed_addr constant [2 x i8] c"!\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1
@.fmt.concat.0 = private unnamed_addr constant [15 x i8] c"Hello, world!\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.0)
  ret i32 0
}

