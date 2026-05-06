; ModuleID = 'InputKt'
source_filename = "InputKt.kt"

@.str.0 = private unnamed_addr constant [8 x i8] c"evens: \00", align 1
@.str.1 = private unnamed_addr constant [7 x i8] c"odds: \00", align 1
@.fmt.int_println = private unnamed_addr constant [4 x i8] c"%d\0A\00", align 1
@.fmt.concat.0 = private unnamed_addr constant [11 x i8] c"evens: %s\0A\00", align 1
@.fmt.concat.1 = private unnamed_addr constant [10 x i8] c"odds: %s\0A\00", align 1

declare i32 @printf(ptr, ...)

define i32 @main() {
entry:
  %t0 = add i32 0, 1
  %t1 = add i32 0, 2
  %t2 = add i32 0, 3
  %t3 = add i32 0, 4
  %t4 = add i32 0, 5
  %t5 = add i32 0, 6
  %t6 = add i32 0, 7
  %t7 = add i32 0, 8
  %t8 = add i32 0, 9
  %t9 = add i32 0, 10
  %t10 = add i32 0, 10
  %t11 = add i32 0, 0
  %t12 = inttoptr i64 0 to ptr
  %t13 = add i32 0, 1
  %t14 = inttoptr i64 0 to ptr
  %t15 = add i32 0, 2
  %t16 = inttoptr i64 0 to ptr
  %t17 = add i32 0, 3
  %t18 = inttoptr i64 0 to ptr
  %t19 = add i32 0, 4
  %t20 = inttoptr i64 0 to ptr
  %t21 = add i32 0, 5
  %t22 = inttoptr i64 0 to ptr
  %t23 = add i32 0, 6
  %t24 = inttoptr i64 0 to ptr
  %t25 = add i32 0, 7
  %t26 = inttoptr i64 0 to ptr
  %t27 = add i32 0, 8
  %t28 = inttoptr i64 0 to ptr
  %t29 = add i32 0, 9
  %t30 = inttoptr i64 0 to ptr
  %t31 = inttoptr i64 0 to ptr
  %t32 = inttoptr i64 0 to ptr
  %t33 = inttoptr i64 0 to ptr
  %t34 = inttoptr i64 0 to ptr
  %t35 = inttoptr i64 0 to ptr
  %t36 = inttoptr i64 0 to ptr
  %t37 = inttoptr i64 0 to ptr
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.0, ptr %t34)
  call i32 (ptr, ...) @printf(ptr @.fmt.concat.1, ptr %t37)
  ret i32 0
}

