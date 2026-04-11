; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [15 x i8] c"The answer is \00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1
@.fmt.concat.0 = private unnamed_addr constant [18 x i8] c"The answer is %d\0A\00", align 1
@.fmt.concat.1 = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %t0 = add i32 0, 42
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t0)
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.0, i32 %t0)
  %t3 = add i32 0, 1
  %t4 = add i32 %t0, %t3
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.1, i32 %t4)
  ret i32 0
}

