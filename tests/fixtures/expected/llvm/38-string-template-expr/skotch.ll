; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [6 x i8] c"sum: \00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1
@.fmt.concat.0 = private unnamed_addr constant [9 x i8] c"sum: %d\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %t0 = add i32 0, 1
  %t1 = add i32 0, 2
  %t2 = add i32 %t0, %t1
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.0, i32 %t2)
  ret i32 0
}

