; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.true = private unnamed_addr constant [5 x i8] c"true\00", align 1
@.str.false = private unnamed_addr constant [6 x i8] c"false\00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1

declare i32 @puts(ptr)
declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %t0 = add i32 0, 65
  %t1 = add i32 0, 10
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t0)
  %t3 = add i32 0, 90
  call i32 (ptr, ...) @printf(ptr @.fmt.int_println, i32 %t3)
  %t5 = add i32 0, 10
  %t6 = icmp eq i32 %t1, %t5
  %t8 = select i1 %t6, ptr @.str.true, ptr @.str.false
  call i32 @puts(ptr %t8)
  ret i32 0
}

